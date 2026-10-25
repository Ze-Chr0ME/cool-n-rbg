//! Daemon <-> GUI protocol: one JSON object per line over the named pipe `\\.\pipe\coolrgb`.
use crate::config::Config;
use crate::devices::evision_v2::{KeyboardChange, KeyboardState};
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
    /// Change a setting stored on a keyboard; the new state shows up in `Status::keyboards`.
    Keyboard { id: String, change: KeyboardChange },
}

/// Device category, used to group the Devices page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    /// Fan/LED hub.
    Controller,
    Memory,
    Motherboard,
    #[default]
    Other,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Keyboard => "Keyboard",
            DeviceKind::Mouse => "Mouse",
            DeviceKind::Controller => "Controller",
            DeviceKind::Memory => "Memory",
            DeviceKind::Motherboard => "Motherboard",
            DeviceKind::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub leds: usize,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub devices: Vec<DeviceInfo>,
    /// Index 0 = pump.
    pub rpm: Vec<u16>,
    /// Duty actually sent, index 0 = pump.
    pub duty: Vec<u8>,
    pub liquid: Option<f32>,
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_fan_rpm: Vec<u32>,
    pub gpu_fan_percent: Vec<u32>,
    pub gpu_fan_manual: bool,
    /// Commander Core LED counts per port as the hub detected them and as actually driven.
    pub cc_ports_detected: Vec<u16>,
    pub cc_ports: Vec<u16>,
    pub keyboards: Vec<KeyboardState>,
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
