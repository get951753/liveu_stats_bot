use std::sync::Arc;
use tokio::sync::Mutex;

use anyhow::{Context, Result};
use liveu_stats_bot::{config::Config, liveu::{Liveu, Battery}, liveu_monitor::{Monitor, Modem}, twitch::Twitch, srt};
use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use serde::Serialize;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Started liveu stats bot v{}", env!("CARGO_PKG_VERSION"));

    let config = match Config::load("config.json") {
        Ok(c) => c,
        Err(_) => Config::ask_for_settings().await?,
    };

    println!("Liveu: Authenticating...");
    let liveu = Liveu::authenticate(config.liveu.clone())
        .await
        .context("Failed to authenticate. Are your login details correct?")?;
    println!("Liveu: Authenticated");

    let liveu_boss_id = if let Some(boss_id) = &config.liveu.id {
        boss_id.to_owned()
    } else {
        let inventories = liveu
            .get_inventories()
            .await
            .context("Error getting inventories")?;
        let loc = Liveu::get_boss_id_location(&inventories);
        inventories.units[loc].id.to_owned()
    };

    {
        let modem_sync = Arc::new(Mutex::new(Vec::new()));
        let battery_sync = Arc::new(Mutex::new(
            Battery {
                connected: false,
                percentage: 255,
                run_time_to_empty: 0,
                discharging: false,
                charging: false,
            }));
        let total_bitrate_sync: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let srt_bitrate_sync: Arc<Mutex<i64>> = Arc::new(Mutex::new(0));
        let srt_bitrate = Arc::clone(&srt_bitrate_sync);

        if let Some(srt) = config.srt.clone() {
            tokio::spawn(async move { srt::srt_bitrate_monitor(&srt, srt_bitrate.clone()).await });
        }

        println!("\nTwitch: Connecting...");
        let (twitch_client, twitch_join_handle) =
            Twitch::run(
                config.clone(), 
                liveu.clone(), 
                liveu_boss_id.to_owned(), 
                Arc::clone(&modem_sync), 
                Arc::clone(&battery_sync), 
                Arc::clone(&srt_bitrate_sync),
            );
        println!("Twitch: Connected");

        {
            let monitor = Monitor {
                client: twitch_client.clone(),
                config: config.clone(),
                liveu: liveu.clone(),
                boss_id: liveu_boss_id.to_owned(),
                lang: config.lang.clone(),
                total_bitrate: Arc::clone(&total_bitrate_sync),
                modem_sync: Arc::clone(&modem_sync),
                battery_sync: Arc::clone(&battery_sync),
            };

            if config.liveu.monitor.modems || config.server {
                if config.liveu.monitor.modems{
                    println!("Liveu: monitoring modems");
                }
                let modems = monitor.clone();
                tokio::spawn(async move { modems.monitor_modems().await });
            }

            if config.liveu.monitor.battery || config.server {
                if config.liveu.monitor.battery{
                    println!("Liveu: monitoring battery");
                }
                let battery = monitor.clone();
                tokio::spawn(async move { battery.monitor_battery().await });
            }

            if config.server {
                let port = 8183;        
                let data = AppState{
                    monitor: monitor.clone(),
                    srt_bitrate: Arc::clone(&srt_bitrate_sync),
                };

                println!("Server is starting on port: {}", port);       
                HttpServer::new(move || {
                    let cors = Cors::default()
                        .allow_any_origin()
                        .allowed_methods(vec!["GET"]);

                    App::new()
                        .app_data(web::Data::new(data.clone()))
                        .route("/stats", web::to(do_get))
                        .wrap(cors)
                })
                .bind(("127.0.0.1", port))?
                .run()
                .await?;
            }
        }
        twitch_join_handle.await?;
    }

    Ok(())
}

async fn do_get(data: web::Data<AppState>) -> web::Json<JsonData> {
    let obj = web::Json(JsonData{
        modems: (data.monitor.modem_sync.lock().await).clone(),
        total_bitrate: *data.monitor.total_bitrate.lock().await,
        srt_bitrate: *data.srt_bitrate.lock().await,
        battery: (data.monitor.battery_sync.lock().await).clone(),
    });  

    obj
}

#[derive(Clone)]
struct AppState {
    monitor: Monitor,
    srt_bitrate: Arc<Mutex<i64>>,
}

#[derive(Serialize)]
struct JsonData {
    modems: Vec<Modem>,
    total_bitrate: u32,
    battery: Battery,
    srt_bitrate: i64,
}