// Pipe client for testing: `ctl status | config | rescan | quit | set <json-config-file>`
use coolrgb_core::ipc::{call, Request};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let req = match args.first().map(String::as_str) {
        Some("config") => Request::GetConfig,
        Some("rescan") => Request::Rescan,
        Some("quit") => Request::Quit,
        Some("handover") => Request::Handover,
        Some("set") => Request::SetConfig {
            config: serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap(),
        },
        _ => Request::Status,
    };
    match call(&req) {
        Ok(r) => println!("{}", serde_json::to_string_pretty(&r).unwrap()),
        Err(e) => println!("error: {e}"),
    }
}
