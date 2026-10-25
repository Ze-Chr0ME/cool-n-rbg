// Read-only NVAPI probe: GPU temperature, fan coolers, and the memory cost of loading nvapi64.dll.
use coolrgb_core::nvapi::NvGpu;

fn private_mb() -> f64 {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("(Get-Process -Id {}).PrivateMemorySize64", std::process::id())])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1048576.0
}

fn main() {
    let before = private_mb();
    let gpu = NvGpu::open().unwrap();
    println!("temp {:?}", gpu.temperature());
    println!("coolers {:?}", gpu.coolers());
    println!("private MB before {before:.1} after {:.1}", private_mb());
    if std::env::args().any(|a| a == "crash") {
        println!("manual 45% then exit without restoring -> {:?}", gpu.set_all(Some(45)));
        std::process::exit(0);
    }
    if std::env::args().any(|a| a == "auto") {
        println!("auto -> {:?}", gpu.set_all(None));
    }
    if std::env::args().any(|a| a == "set") {
        println!("manual 60% -> {:?}", gpu.set_all(Some(60)));
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            println!("  {:?}", gpu.coolers());
        }
        println!("auto -> {:?}", gpu.set_all(None));
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!("  {:?}", gpu.coolers());
    }
}
