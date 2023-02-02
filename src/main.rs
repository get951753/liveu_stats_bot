use anyhow::{Context, Result};
use liveu_stats_bot::{config::Config, liveu::{Liveu, Battery}, liveu_monitor::{Monitor, Modem}, twitch::Twitch, srt};
use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use serde::Serialize;

#[derive(Clone)]
struct AppState {
    monitor: Monitor,
    config: Config,
}

#[derive(Serialize)]
struct JsonData {
    modems: Vec<Modem>,
    total_bitrate: u32,
    battery: Battery,
    srt_bitrate: i64,
}

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

    println!("\nTwitch: Connecting...");
    let (twitch_client, twitch_join_handle) =
        Twitch::run(config.clone(), liveu.clone(), liveu_boss_id.to_owned());
    println!("Twitch: Connected");

    {
        let monitor = Monitor {
            client: twitch_client.clone(),
            config: config.clone(),
            liveu: liveu.clone(),
            boss_id: liveu_boss_id.to_owned(),
        };

        if config.liveu.monitor.modems {
            println!("Liveu: monitoring modems");
            let modems = monitor.clone();
            tokio::spawn(async move { modems.monitor_modems().await });
        }

        if config.liveu.monitor.battery {
            println!("Liveu: monitoring battery");
            let battery = monitor.clone();
            tokio::spawn(async move { battery.monitor_battery().await });
        }

        if config.server{
            let port = 8183;            
            let data = AppState{
                monitor: monitor.clone(),
                config: config.clone(),
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

    Ok(())
}

async fn do_get(data: web::Data<AppState>) -> web::Json<JsonData> {
    let (modems_data, total_bitrate) = data.monitor.get_modems_data().await;
    let battery_data = data.monitor.get_battery_data().await;
    let mut srt_bitrate: i64 = 0;
    if let Some(srt) = &data.config.srt {
        if let Ok(bitrate) = srt::get_srt_bitrate(srt).await {
            srt_bitrate = bitrate;
        };
    }

    let obj = web::Json(JsonData{
        modems: modems_data,
        total_bitrate: total_bitrate,
        srt_bitrate: srt_bitrate,
        battery: battery_data,
    });  
    
    obj
}