// Diagnostic: light each Commander Core LED port a distinct color for N seconds.
// `cc_ports [seconds]`.
use coolrgb_core::devices::corsair::commander_core::CommanderCore;
use coolrgb_core::effects::Rgb;

const COLORS: [Rgb; 7] = [
    Rgb(255, 255, 255),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(0, 0, 255),
    Rgb(255, 255, 0),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
];

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(90);
    let api = hidapi::HidApi::new().unwrap();
    let mut cc = CommanderCore::open(&api, &[]).unwrap().unwrap();
    println!("detected {:?} driven {:?}", cc.detected, cc.port_leds);
    let mut buf = Vec::new();
    for (i, &n) in cc.port_leds.iter().enumerate() {
        buf.extend(std::iter::repeat(COLORS[i]).take(n as usize));
    }
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        cc.set_colors(&buf).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
