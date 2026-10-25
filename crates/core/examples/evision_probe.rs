//! Read-only check: open the EVision keyboard and print its slot count. `--color` also flashes
//! red/green/blue for 1 s each, then hands the keyboard back to its stored effect.
use coolrgb_core::devices::evision_v2::EvisionV2;
use coolrgb_core::effects::Rgb;
use std::time::{Duration, Instant};

fn main() {
    let api = hidapi::HidApi::new().unwrap();
    let color = std::env::args().any(|a| a == "--color");
    for d in EvisionV2::open_all(&api) {
        let mut d = match d {
            Ok(d) => d,
            Err(e) => {
                println!("error: {e}");
                continue;
            }
        };
        println!("{}: {} slots, {} columns", d.name, d.slots, d.columns());
        if std::env::args().any(|a| a == "--caps") {
            println!("caps: {:02x?}", d.read_caps());
        }
        if std::env::args().any(|a| a == "--dump") {
            let end = std::env::args().skip_while(|a| a != "--dump").nth(1).and_then(|v| u16::from_str_radix(&v, 16).ok()).unwrap_or(0x200);
            match d.read_config(0, end as usize) {
                Ok(cfg) => {
                    for (i, row) in cfg.chunks(16).enumerate() {
                        println!("{:04x}: {}", i * 16, row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
                    }
                }
                Err(e) => println!("read config: {e}"),
            }
        }
        if !color {
            d.restore_on_drop = false;
            continue;
        }
        for c in [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)] {
            println!("{:?} -> {:?}", c, d.set_columns(&vec![c; d.columns()]));
            let end = Instant::now() + Duration::from_secs(1);
            while Instant::now() < end {
                std::thread::sleep(Duration::from_millis(100));
                if let Err(e) = d.refresh() {
                    println!("refresh: {e}");
                }
            }
        }
    }
}
