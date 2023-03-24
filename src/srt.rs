use std::sync::Arc;
use tokio::sync::Mutex;

use serde::Deserialize;

use crate::{config, error::Error};

use serde_json::Value;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stat {
    pub bitrate: i64,
    pub bytes_rcv_drop: u64,
    pub bytes_rcv_loss: u64,
    pub mbps_bandwidth: f64,
    pub mbps_recv_rate: f64,
    pub ms_rcv_buf: i32,
    pub pkt_rcv_drop: i32,
    pub pkt_rcv_loss: i32,
    pub rtt: f64,
    pub uptime: i64,
}

pub async fn get_srt_bitrate(config: &config::Srt) -> Result<i64, Error> {
    let res = reqwest::get(&config.url).await?;

    if res.status() != reqwest::StatusCode::OK {
        return Err(Error::SrtDown("Can't connect to SRT stats".to_owned()));
    }

    let text = res.text().await?;
    let data: Value = serde_json::from_str(&text)?;

    let publisher = &data["publishers"][&config.publisher];

    let stream: Stat = serde_json::from_value(publisher.to_owned())?;

    Ok(stream.bitrate)
}

pub async fn srt_bitrate_monitor(config: &config::Srt, srt_bitrate_sync: Arc<Mutex<i64>>){
    loop{
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        if let Ok(bitrate) = get_srt_bitrate(config).await{
            let mut srt_bitrate = srt_bitrate_sync.lock().await;
            *srt_bitrate = bitrate;
        }else{
            let mut srt_bitrate = srt_bitrate_sync.lock().await;
            *srt_bitrate = 0; 
        }
    }
}
