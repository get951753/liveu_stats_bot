use crate::{
    config,
    error::Error,
    liveu::{self, Liveu},
    nginx,
    srt,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use twitch_irc::{
    login::StaticLoginCredentials,
    message,
    transport::tcp::{TCPTransport, TLS},
    ClientConfig, TwitchIRCClient,
};

const OFFLINE_MSG: &str = "LiveU Offline :(";

pub struct Twitch {
    client: TwitchIRCClient<TCPTransport<TLS>, StaticLoginCredentials>,
    liveu: Liveu,
    liveu_boss_id: String,
    config: config::Config,
    timeout: Arc<AtomicBool>,
}

impl Twitch {
    pub fn run(
        config: config::Config,
        liveu: Liveu,
        liveu_boss_id: String,
    ) -> (
        TwitchIRCClient<TCPTransport<TLS>, StaticLoginCredentials>,
        tokio::task::JoinHandle<()>,
    ) {
        let config::Twitch {
            bot_username,
            bot_oauth,
            channel,
            mod_only,
            ..
        } = &config.twitch;

        let username = bot_username.to_lowercase();
        let channel = channel.to_lowercase();
        let mut oauth = bot_oauth.to_owned();

        if let Some(strip_oauth) = oauth.strip_prefix("oauth:") {
            oauth = strip_oauth.to_string();
        }

        let twitch_credentials = StaticLoginCredentials::new(username, Some(oauth));
        let twitch_config = ClientConfig::new_simple(twitch_credentials);
        let (mut incoming_messages, client) =
            TwitchIRCClient::<TCPTransport<TLS>, StaticLoginCredentials>::new(twitch_config);

        client.join(channel);

        let mod_only = mod_only.to_owned();
        let client_clone = client.clone();
        let join_handler = tokio::spawn(async move {
            let t = Self {
                client: client_clone,
                liveu,
                liveu_boss_id,
                config,
                timeout: Arc::new(AtomicBool::new(false)),
            };

            while let Some(message) = incoming_messages.recv().await {
                t.handle_chat(message, &mod_only).await;
            }
        });

        (client, join_handler)
    }

    async fn handle_chat(&self, message: message::ServerMessage, mod_only: &bool) {
        let timeout = self.timeout.clone();
        if timeout.load(Ordering::Acquire) {
            return;
        }

        match message {
            message::ServerMessage::Notice(msg) => {
                if msg.message_text == "Login authentication failed" {
                    panic!("Twitch authentication failed");
                }
            }
            message::ServerMessage::Privmsg(msg) => {
                let is_owner = msg.badges.contains(&twitch_irc::message::Badge {
                    name: "broadcaster".to_string(),
                    version: "1".to_string(),
                });

                let is_mod = msg.badges.contains(&twitch_irc::message::Badge {
                    name: "moderator".to_string(),
                    version: "1".to_string(),
                });

                let mut user_has_permission = false;

                if let Some(users) = &self.config.twitch.admin_users {
                    for user in users {
                        if user.to_lowercase() == msg.sender.login {
                            user_has_permission = true;
                            break;
                        }
                    }
                };

                if *mod_only && !(is_owner || is_mod || user_has_permission) {
                    return;
                }

                let command = msg
                    .message_text
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();

                let command = self.get_command(command);

                if command == Command::Unknown {
                    return;
                }

                let cooldown = self.config.commands.cooldown;

                tokio::spawn(async move {
                    timeout.store(true, Ordering::Release);
                    tokio::time::sleep(tokio::time::Duration::from_secs(cooldown as u64)).await;
                    timeout.store(false, Ordering::Release);
                });

                let res = if command == Command::Stats || command == Command::Battery {
                    self.handle_non_permission_commands(command).await
                } else {
                    if !(is_owner || user_has_permission) {
                        return;
                    }

                    self.handle_permission_commands(command, msg.channel_login.to_owned())
                        .await
                };

                if let Ok(res) = res {
                    let _ = self.client.say(msg.channel_login.to_owned(), res).await;
                }
            }
            _ => {}
        };
    }

    async fn handle_non_permission_commands(&self, command: Command) -> Result<String, Error> {
        match command {
            Command::Stats => self.generate_liveu_modems_message().await,
            Command::Battery => self.generate_liveu_battery_message().await,
            _ => unreachable!(),
        }
    }

    async fn handle_permission_commands(
        &self,
        command: Command,
        channel: String,
    ) -> Result<String, Error> {
        match command {
            Command::Start => self.generate_liveu_start_message(channel).await,
            Command::Stop => self.generate_liveu_stop_message(channel).await,
            Command::Restart => self.generate_liveu_restart_message(channel).await,
            Command::Reboot => self.generate_liveu_reboot_message(channel).await,
            Command::Delay => self.toggle_delay(channel).await,
            _ => unreachable!(),
        }
    }

    // TODO: This needs a refactor
    fn get_command(&self, command: String) -> Command {
        let config::Commands {
            stats,
            battery,
            start,
            stop,
            restart,
            reboot,
            delay,
            ..
        } = &self.config.commands;

        if stats.contains(&command) {
            return Command::Stats;
        }

        if battery.contains(&command) {
            return Command::Battery;
        }

        if start == &command {
            return Command::Start;
        }

        if stop == &command {
            return Command::Stop;
        }

        if restart == &command {
            return Command::Restart;
        }

        if reboot == &command {
            return Command::Reboot;
        }

        if delay == &command {
            return Command::Delay;
        }

        Command::Unknown
    }

    async fn generate_liveu_modems_message(&self) -> Result<String, Error> {
        let interfaces: Vec<liveu::Interface> = self
            .liveu
            .get_unit_custom_names(&self.liveu_boss_id, self.config.custom_port_names.clone())
            .await?;

        if interfaces.is_empty() {
            return Ok(OFFLINE_MSG.to_string());
        }

        let mut message = String::new();
        let mut total_bitrate = 0;

        for interface in interfaces.iter() {
            message = message
                + &format!(
                    "{}: {} Kbps{}{}, ",
                    interface.port,
                    interface.uplink_kbps,
                    if !interface.technology.is_empty() {
                        format!(" ({})", &interface.technology)
                    } else {
                        "".to_string()
                    },
                    if interface.is_currently_roaming {
                        " roaming"
                    } else {
                        ""
                    }
                );
            total_bitrate += interface.uplink_kbps;
        }

        if total_bitrate == 0 {
            return Ok("LiveU Online and Ready".to_string());
        }

        message += &format!("Total LRT: {} Kbps", total_bitrate);

        if let Some(srt) = &self.config.srt {
            if let Ok(bitrate) = srt::get_srt_bitrate(srt).await {
                message += &format!(", SRT: {} Kbps", bitrate);
            };
        }
        if let Some(rtmp) = &self.config.rtmp {
            if let Ok(Some(bitrate)) = nginx::get_rtmp_bitrate(rtmp).await {
                message += &format!(", RTMP: {} Kbps", bitrate);
            };
        }

        Ok(message)
    }

    async fn generate_liveu_battery_message(&self) -> Result<String, Error> {
        let battery = match self.liveu.get_battery(&self.liveu_boss_id).await {
            Ok(b) => b,
            Err(_) => return Ok(OFFLINE_MSG.to_string()),
        };

        let estimated_battery_time = {
            if battery.run_time_to_empty != 0 && battery.discharging {
                let hours = battery.run_time_to_empty / 60;
                let minutes = battery.run_time_to_empty % 60;
                let mut time_string = String::new();

                if hours != 0 {
                    time_string += &format!("{}h", hours);
                }

                time_string += &format!(" {}m", minutes);
                format!("Estimated battery time: {}", time_string)
            } else {
                "".to_string()
            }
        };

        let charging = {
            if battery.charging {
                "charging".to_string()
            } else if battery.percentage == 100 {
                let mut s = "fully charged".to_string();

                if battery.connected {
                    s += ", connected"
                }

                s
            } else if battery.percentage < 100 && !battery.charging && !battery.discharging {
                "too hot to charge".to_string()
            } else {
                "not charging".to_string()
            }
        };

        let message = format!(
            "LiveU Internal Battery: {}% {} {}",
            battery.percentage, charging, estimated_battery_time
        );

        Ok(message)
    }

    async fn generate_liveu_start_message(&self, channel: String) -> Result<String, Error> {
        let video = self.liveu.get_video(&self.liveu_boss_id).await;

        let video = match video {
            Ok(video) => video,
            Err(_) => return Ok(OFFLINE_MSG.to_string()),
        };

        if video.resolution.is_none() {
            return Ok("LiveU no camera plugged in".to_string());
        }

        if video.bitrate.is_some() {
            return Ok("LiveU already streaming".to_string());
        }

        if self.liveu.start_stream(&self.liveu_boss_id).await.is_err() {
            return Ok("LiveU request error".to_string());
        };

        let confirm = DataUsedInThread {
            chat: self.client.clone(),
            liveu: self.liveu.clone(),
            boss_id: self.liveu_boss_id.to_owned(),
            channel,
        };

        tokio::spawn(async move {
            confirm
                .confirm_action(15, true, "started".to_string(), "starting".to_string())
                .await
        });

        Ok("LiveU starting stream".to_string())
    }

    async fn generate_liveu_stop_message(&self, channel: String) -> Result<String, Error> {
        if !self.liveu.is_streaming(&self.liveu_boss_id).await {
            return Ok("LiveU already stopped".to_string());
        }

        if self.liveu.stop_stream(&self.liveu_boss_id).await.is_err() {
            return Ok("LiveU request error".to_string());
        };

        let confirm = DataUsedInThread {
            chat: self.client.clone(),
            liveu: self.liveu.clone(),
            boss_id: self.liveu_boss_id.to_owned(),
            channel,
        };

        tokio::spawn(async move {
            confirm
                .confirm_action(10, false, "stopped".to_string(), "stopping".to_string())
                .await
        });

        Ok("LiveU stopping stream".to_string())
    }

    async fn generate_liveu_restart_message(&self, channel: String) -> Result<String, Error> {
        if !self.liveu.is_streaming(&self.liveu_boss_id).await {
            return Ok("LiveU not streaming".to_string());
        }

        let msg = "LiveU stream restarting".to_string();
        let _ = self.client.say(channel.to_owned(), msg).await;

        self.generate_liveu_stop_message(channel.to_owned()).await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        self.generate_liveu_start_message(channel.to_owned())
            .await?;

        Ok(String::new())
    }

    async fn generate_liveu_reboot_message(&self, channel: String) -> Result<String, Error> {
        let is_streaming = self.liveu.is_streaming(&self.liveu_boss_id).await;

        let msg = "LiveU Rebooting, please wait approximately 2-3 minutes".to_string();
        let _ = self.client.say(channel.to_owned(), msg).await;

        if is_streaming {
            self.generate_liveu_stop_message(channel.to_owned()).await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        }

        self.liveu.reboot_unit(&self.liveu_boss_id).await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

        let mut attempts = 0;
        let max_attempts = 20;

        while !self.liveu.is_idle(&self.liveu_boss_id).await && attempts != max_attempts {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            attempts += 1;
        }

        if attempts == max_attempts {
            return Ok("LiveU took too long to reboot".to_string());
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        if is_streaming {
            self.generate_liveu_start_message(channel.to_owned())
                .await?;
            return Ok(String::new());
        }

        Ok("LiveU rebooted successfully".to_string())
    }

    async fn toggle_delay(&self, channel: String) -> Result<String, Error> {
        let is_streaming = self.liveu.is_streaming(&self.liveu_boss_id).await;

        if is_streaming {
            self.generate_liveu_stop_message(channel.to_owned()).await?;
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        }

        let current_delay = self.liveu.get_delay(&self.liveu_boss_id).await?;
        let delay = if current_delay.delay == 1000 {
            (5000, "LiveU high resiliency mode")
        } else {
            (1000, "LiveU low delay mode")
        };

        self.liveu.set_delay(&self.liveu_boss_id, delay.0).await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        if is_streaming {
            self.generate_liveu_start_message(channel.to_owned())
                .await?;
        }

        Ok(delay.1.to_string())
    }
}

#[derive(PartialEq, Eq)]
enum Command {
    Stats,
    Battery,
    Start,
    Stop,
    Restart,
    Reboot,
    Delay,
    Unknown,
}

struct DataUsedInThread {
    chat: TwitchIRCClient<TCPTransport<TLS>, StaticLoginCredentials>,
    liveu: Liveu,
    boss_id: String,
    channel: String,
}

impl DataUsedInThread {
    async fn confirm_action(
        &self,
        max_attempts: u8,
        should_have_bitrate: bool,
        success: String,
        not_success: String,
    ) {
        let mut attempts = 0;

        while attempts != max_attempts {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let video = self.liveu.get_video(&self.boss_id).await;

            if let Ok(video) = video {
                if video.bitrate.is_some() == should_have_bitrate {
                    break;
                }
            }

            attempts += 1;
        }

        if attempts == max_attempts {
            let msg = format!(
                "LiveU {} stream took too long might not have worked",
                not_success
            );
            let _ = self.chat.say(self.channel.to_owned(), msg).await;

            return;
        }

        let msg = format!("LiveU streaming {} successfully", success);
        let _ = self.chat.say(self.channel.to_owned(), msg).await;
    }
}
