// Crash-safety check. `cc_crash set` sets fans to 45% then exits without restoring hardware mode.
// `cc_crash read` reads RPM right after re-entering software mode (before any speed command).
use coolrgb_core::devices::commander_core::CommanderCore;

fn main() {
    let api = hidapi::HidApi::new().unwrap();
    let cc = CommanderCore::open(&api, &[]).unwrap().unwrap();
    match std::env::args().nth(1).as_deref() {
        Some("set") => {
            cc.set_duty(&[70, 45, 45, 45, 45, 45, 45]).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(8));
            println!("set: {:?}", cc.sensors().unwrap());
            std::process::exit(0); // skips Drop: simulates a crash
        }
        _ => {
            println!("read: {:?}", cc.sensors().unwrap());
            std::process::exit(0);
        }
    }
}
