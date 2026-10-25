//! Daemon <-> GUI protocol: one JSON object per line over the named pipe `\\.\pipe\coolrgb`.
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};

pub const PIPE_NAME: &str = r"\\.\pipe\coolrgb";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    GetConfig,
    SetConfig { config: Config },
    Rescan,
    Quit,
    /// Quit without handing devices back to their hardware lighting (a new daemon takes over right away).
    Handover,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub leds: usize,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub devices: Vec<DeviceInfo>,
    /// Index 0 = pump.
    pub rpm: Vec<u16>,
    /// Duty actually sent, index 0 = pump.
    pub duty: Vec<u8>,
    pub liquid: Option<f32>,
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub messages: Vec<String>,
    pub working_set_kb: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Status { status: Status },
    Config { config: Config },
    Ok,
    Error { message: String },
}

/// Client side: one request per connection.
pub fn call(req: &Request) -> Result<Response, String> {
    // The daemon serves one client at a time; retry briefly while the pipe is busy.
    let mut attempt = 0;
    let mut f = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => break f,
            Err(e) if attempt < 10 && e.raw_os_error() == Some(231) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Err(e) => return Err(format!("daemon not reachable: {e}")),
        }
    };
    let mut line = serde_json::to_string(req).unwrap();
    line.push('\n');
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    BufReader::new(f).read_line(&mut resp).map_err(|e| e.to_string())?;
    serde_json::from_str(&resp).map_err(|e| format!("bad response: {e}"))
}
