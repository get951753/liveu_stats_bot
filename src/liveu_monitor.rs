use std::sync::Arc;

use tokio::sync::Mutex;
use twitch_irc::{
    login,
    transport::tcp::{TCPTransport, TLS},
    TwitchIRCClient,
};
use serde::Serialize;
use rust_i18n::t;

use crate::{config, liveu};

#[derive(Debug, Clone, Serialize)]
pub struct Modem {
    pub port: String,
    pub uplink_kbps: u32,
    pub connected: bool,
    pub enabled: bool,
    pub technology: String,
    pub is_currently_roaming: bool,
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub client: TwitchIRCClient<TCPTransport<TLS>, login::StaticLoginCredentials>,
    pub config: config::Config,
    pub liveu: liveu::Liveu,
    pub boss_id: String,
    pub lang: String,
    pub total_bitrate: Arc<Mutex<u32>>,
    pub modem_sync: Arc<Mutex<Vec<Modem>>>,
    pub battery_sync: Arc<Mutex<liveu::Battery>>,
}

impl Monitor {
    pub fn run(&self) {
        let modems = self.clone();
        tokio::spawn(async move { modems.monitor_modems().await });

        let battery = self.clone();
        tokio::spawn(async move { battery.monitor_battery().await });
    }

    pub async fn monitor_modems(&self) {
        let mut current_modems = Vec::new();
        let mut ignore = false;

        for interface in self
            .liveu
            .get_unit_custom_names(&self.boss_id, self.config.custom_port_names.clone())
            .await
            .unwrap()
        {
            current_modems.push(interface.port);
        }

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            if !self.liveu.is_streaming(&self.boss_id).await {
                let mut modem_sync = self.modem_sync.lock().await;
                let mut total_bitrate = self.total_bitrate.lock().await;
                *total_bitrate = 0;
                *modem_sync = Vec::new();
                ignore = true;
                continue;
            }

            let mut current = Vec::new();
            let mut new_modems = Vec::new();
            {
                let interfaces = self
                    .liveu
                    .get_unit_custom_names(&self.boss_id, self.config.custom_port_names.clone())
                    .await
                    .unwrap();
                let mut modem_sync = self.modem_sync.lock().await;
                let mut total_bitrate = self.total_bitrate.lock().await;
                *total_bitrate = 0;
                *modem_sync = Vec::new();
                
                for interface in interfaces{  
                    *total_bitrate += &interface.uplink_kbps;
                    (*modem_sync).push(Modem{
                        port: interface.port.to_string(), 
                        connected: interface.connected, 
                        uplink_kbps: interface.uplink_kbps, 
                        enabled: interface.enabled,
                        technology: interface.technology,
                        is_currently_roaming: interface.is_currently_roaming,
                    });
                    // we got a new interface
                    if !current_modems.contains(&interface.port) {
                        // println!("New modem {}", interface.port);
                        new_modems.push(interface.port.to_owned());
                        current_modems.push(interface.port.to_owned());
                    }

                    current.push(interface.port);
                }
            }
            // check diff between current and prev
            let mut removed_modems = Vec::new();
            for modem in current_modems.iter() {
                if !current.contains(modem) {
                    // println!("Removed modem {}", modem);
                    removed_modems.push(modem.to_owned());
                }
            }

            for rem in removed_modems.iter() {
                let index = current_modems.iter().position(|m| m == rem).unwrap();
                current_modems.swap_remove(index);
            }

            let message = Self::generate_modems_message(new_modems, removed_modems, self.lang.clone());

            if !ignore && !message.is_empty() {
                let _ = self
                    .client
                    .say(
                        self.config.twitch.channel.to_owned(),
                        "LiveU: ".to_string() + &message,
                    )
                    .await;
            }

            if ignore {
                ignore = false;
            }
        }
    }

    fn generate_modems_message(new_modems: Vec<String>, removed_modems: Vec<String>, lang: String) -> String {
        let mut message = String::new();

        if !new_modems.is_empty() {
            let a = if new_modems.len() > 1 { t!("monitor.are", locale = &lang) } else { t!("monitor.is", locale = &lang) };

            message += t!(
                "monitor.new_modem", 
                locale = &lang, 
                newModems = &new_modems.join(", "), 
                isORare = &a
            ).as_str();
        }

        if !removed_modems.is_empty() {
            let a = if removed_modems.len() > 1 {
                t!("monitor.have", locale = &lang)
            } else {
                t!("monitor.has", locale = &lang)
            };

            message += t!(
                "monitor.remove_modem", 
                locale = &lang, 
                removedModems = &removed_modems.join(", "), 
                haveORhas = &a
            ).as_str();
        }

        message
    }

    pub async fn monitor_battery(&self) {
        let mut prev = liveu::Battery {
            connected: false,
            percentage: 255,
            run_time_to_empty: 0,
            discharging: false,
            charging: false,
        };

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            if !self.liveu.is_streaming(&self.boss_id).await {
                let mut battery_sync = self.battery_sync.lock().await;
                *battery_sync = liveu::Battery {
                    connected: false,
                    percentage: 255,
                    run_time_to_empty: 0,
                    discharging: false,
                    charging: false,
                };
                continue;
            }

            let battery = if let Ok(battery) = self.liveu.get_battery(&self.boss_id).await {
                let mut battery_sync = self.battery_sync.lock().await;
                *battery_sync = battery.clone();

                if prev.percentage == 255 {
                    prev = battery.clone();
                }

                battery
            } else {
                let mut battery_sync = self.battery_sync.lock().await;
                *battery_sync = liveu::Battery {
                    connected: false,
                    percentage: 255,
                    run_time_to_empty: 0,
                    discharging: false,
                    charging: false,
                };

                continue;
            };

            if !battery.charging && battery.discharging && !prev.discharging {
                let _ = self
                    .client
                    .say(
                        self.config.twitch.channel.to_owned(),
                        t!("monitor.rip_power", locale = &self.lang),
                    )
                    .await;
            }

            if battery.charging && !battery.discharging && !prev.charging {
                let _ = self
                    .client
                    .say(
                        self.config.twitch.channel.to_owned(),
                        t!("monitor.now_charging", locale = &self.lang),
                    )
                    .await;
            }

            if battery.percentage < 100
                && !battery.charging
                && !battery.discharging
                && (prev.charging || prev.discharging)
            {
                let _ = self
                    .client
                    .say(
                        self.config.twitch.channel.to_owned(),
                        t!("monitor.too_hot", locale = &self.lang),
                    )
                    .await;
            }

            if battery.percentage == 100
                && !battery.charging
                && !battery.discharging
                && prev.charging
                && !prev.discharging
            {
                let _ = self
                    .client
                    .say(
                        self.config.twitch.channel.to_owned(),
                        t!("monitor.fully_charged", locale = &self.lang),
                    )
                    .await;
            }

            for percentage in &self.config.liveu.monitor.battery_notification {
                self.battery_percentage_message(*percentage, &battery, &prev)
                    .await;
            }

            prev = battery;
        }
    }

    pub async fn battery_percentage_message(
        &self,
        percentage: u8,
        current: &liveu::Battery,
        prev: &liveu::Battery,
    ) {
        if current.percentage == percentage && prev.percentage > percentage {
            let a = if current.charging { t!("monitor.charging", locale = &self.lang)} else { t!("monitor.not_charging", locale = &self.lang)};

            let message = t!(
                "monitor.battery_percentage", 
                locale = &self.lang,
                percent = &percentage.to_string(),
                chargingORnot = &a
            );

            let _ = self
                .client
                .say(self.config.twitch.channel.to_owned(), message)
                .await;
        }
    }
}
