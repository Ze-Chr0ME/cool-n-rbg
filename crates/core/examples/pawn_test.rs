// PawnIO-backed devices: CPU temp, GPU temp, DRAM detection + color, MSI-RGB.
use coolrgb_core::devices::{dram::CorsairDram, msi_rgb::MsiRgb, smbus::Smbus};
use coolrgb_core::effects::Rgb;
use coolrgb_core::nvapi::NvGpu;
use coolrgb_core::sensors::{board_name, CpuTemp};
use std::sync::Arc;

fn main() {
    let board = board_name();
    println!("board: {board:?}");
    match CpuTemp::open() { Ok(c) => println!("cpu: {:?}", c.read()), Err(e) => println!("cpu err {e}") }
    match NvGpu::open() { Ok(g) => println!("gpu: {:?}", g.temperature()), Err(e) => println!("gpu err {e}") }
    let write = std::env::args().any(|a| a == "write");
    match Smbus::open() {
        Ok(bus) => {
            let bus = Arc::new(bus);
            let dimms = CorsairDram::detect(&bus);
            for d in &dimms {
                println!("dram {:#04x} pid {:#06x} protocol {}", d.addr, d.pid, d.protocol);
                if write && d.supports_direct() {
                    for c in [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 160, 255)] {
                        println!("  set {:?} -> {:?}", c, d.set_colors(&[c; 10]));
                        std::thread::sleep(std::time::Duration::from_millis(700));
                    }
                }
            }
            if dimms.is_empty() { println!("no corsair dram found"); }
        }
        Err(e) => println!("smbus err {e}"),
    }
    match MsiRgb::detect(&board) {
        Ok(Some(m)) => {
            println!("msi-rgb ok");
            if write { println!("  set -> {:?}", m.set_color(Rgb(0, 160, 255))); }
        }
        Ok(None) => println!("msi-rgb: board not listed"),
        Err(e) => println!("msi-rgb err {e}"),
    }
}
