//! Passive: print every reply the EVision keyboard sends on its vendor collection for N seconds
//! (default 90), except dynamic-color traffic. Every open handle receives every reply, so this shows
//! what another program (the stock ICL03.exe) asks the keyboard. Sends nothing.
use std::time::{Duration, Instant};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(90);
    let api = hidapi::HidApi::new().unwrap();
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == 0x320f && d.usage_page() == 0xff1c && d.usage() == 0x92)
        .expect("keyboard not found");
    let dev = info.open_device(&api).unwrap();
    println!("listening {secs} s");
    let end = Instant::now() + Duration::from_secs(secs);
    let mut r = [0u8; 64];
    while Instant::now() < end {
        let n = dev.read_timeout(&mut r, 100).unwrap_or(0);
        if n == 0 || r[3] == 0x12 {
            continue;
        }
        let len = (r[4] as usize).min(56);
        println!(
            "+{:>6} ms cmd {:02x} off {:04x} size {:02} status {:02x} data {:02x?}",
            (Instant::now() + Duration::from_secs(secs) - end).as_millis(),
            r[3],
            u16::from_le_bytes([r[5], r[6]]),
            r[4],
            r[7],
            &r[8..8 + len]
        );
    }
}
