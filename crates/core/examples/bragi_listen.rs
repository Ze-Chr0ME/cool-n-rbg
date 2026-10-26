//! Prints the Harpoon's event interface (usage page FF42, usage 2) and its current DPI / stage for 40 s.
//! Press the mouse buttons while it runs.
use hidapi::HidApi;
use std::time::{Duration, Instant};
fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ") }
fn main() {
    let api = HidApi::new().unwrap();
    let find = |usage| api.device_list().find(|d| d.vendor_id() == 0x1b1c && d.product_id() == 0x1b5e && d.usage_page() == 0xff42 && d.usage() == usage).unwrap();
    let cmd = find(1).open_device(&api).unwrap();
    let ev = find(2).open_device(&api).unwrap();
    let get = |id: u8| {
        let mut w = [0u8; 65];
        w[1] = 0x08;
        w[2] = 0x02;
        w[3] = id;
        cmd.write(&w).unwrap();
        let mut r = [0u8; 64];
        for _ in 0..5 {
            r = [0u8; 64];
            let n = cmd.read_timeout(&mut r, 100).unwrap_or(0);
            if n > 0 && r[1] == 0x02 { break; }
        }
        u32::from_le_bytes([r[3], r[4], r[5], r[6]])
    };
    // Status of a write to the active-stage register in software mode (same value, so nothing changes).
    let idx = get(0x1e);
    let mut w = [0u8; 65];
    w[1..6].copy_from_slice(&[0x08, 0x01, 0x1e, 0x00, idx as u8]);
    cmd.write(&w).unwrap();
    let mut r = [0u8; 64];
    let _ = cmd.read_timeout(&mut r, 100);
    println!("set 1e={idx} in software mode: status {:02x}", r[2]);
    let end = Instant::now() + Duration::from_secs(40);
    let mut last = Instant::now() - Duration::from_secs(1);
    while Instant::now() < end {
        let mut b = [0u8; 64];
        if let Ok(n) = ev.read_timeout(&mut b, 50) {
            if n > 0 { println!("event {}", hex(&b[..n.min(12)])); }
        }
        if last.elapsed() >= Duration::from_secs(1) {
            last = Instant::now();
            println!("dpi {} stage {}", get(0x20), get(0x1e));
        }
    }
}
