//! Device worker threads. Each thread owns its devices, renders effects from the shared clock and
//! re-opens devices after errors (USB replug, resume from sleep).
use coolrgb_core::config::{Config, FanMode, TempSource, MIN_FAN_PERCENT, MIN_PUMP_PERCENT};
use coolrgb_core::devices::commander_core::CommanderCore;
use coolrgb_core::devices::corsair_v2::CorsairV2;
use coolrgb_core::devices::dram::{self, CorsairDram};
use coolrgb_core::devices::msi_rgb::MsiRgb;
use coolrgb_core::devices::smbus::Smbus;
use coolrgb_core::effects::{Effect, Rgb};
use coolrgb_core::ipc::{DeviceInfo, Status};
use coolrgb_core::sensors::{board_name, CpuTemp, GpuTemp};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(50);
/// SMBus transfers busy-wait in the kernel, so DIMMs animate slower than USB devices.
const DRAM_FRAME: Duration = Duration::from_millis(100);
const IDLE: Duration = Duration::from_millis(250);
const KEEPALIVE: Duration = Duration::from_secs(4);
const LIQUID_CRITICAL: f32 = 50.0;
const CPU_CRITICAL: f32 = 95.0;

pub struct Shared {
    pub config: Mutex<Config>,
    pub config_gen: AtomicU64,
    pub status: Mutex<Status>,
    pub stop: AtomicBool,
    pub rescan_gen: AtomicU64,
    pub start: Instant,
    /// Set by a handover quit: leave devices in software mode for the next daemon.
    pub handover: AtomicBool,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Shared> {
        Arc::new(Shared {
            config: Mutex::new(config),
            config_gen: AtomicU64::new(1),
            status: Mutex::new(Status::default()),
            stop: AtomicBool::new(false),
            rescan_gen: AtomicU64::new(0),
            start: Instant::now(),
            handover: AtomicBool::new(false),
        })
    }

    fn t(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn set_device(&self, info: DeviceInfo) {
        let mut st = self.status.lock().unwrap();
        match st.devices.iter_mut().find(|d| d.id == info.id) {
            Some(d) => *d = info,
            None => st.devices.push(info),
        }
    }

    fn remove_device(&self, id: &str) {
        self.status.lock().unwrap().devices.retain(|d| d.id != id);
    }

    /// Copy of the config when it changed since `gen`.
    fn config_if_changed(&self, gen: &mut u64) -> Option<Config> {
        let g = self.config_gen.load(Ordering::Acquire);
        (g != *gen).then(|| {
            *gen = g;
            self.config.lock().unwrap().clone()
        })
    }

    /// Sleep in small steps so shutdown stays responsive.
    fn nap(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.stopped() {
            let now = Instant::now();
            if now >= end {
                break;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(100)));
        }
    }
}

fn info(id: &str, name: &str, leds: usize, ok: bool, detail: impl Into<String>) -> DeviceInfo {
    DeviceInfo { id: id.into(), name: name.into(), leds, ok, detail: detail.into() }
}

fn brightness(cfg: &Config) -> f32 {
    cfg.brightness.min(100) as f32 / 100.0
}

/// Fan/pump duty with critical-temperature override and floor. Missing sensors mean full speed.
fn target_duty(mode: &FanMode, liquid: Option<f32>, cpu: Option<f32>, gpu: Option<f32>, floor: u8) -> u8 {
    if liquid.is_some_and(|t| t >= LIQUID_CRITICAL) || cpu.is_some_and(|t| t >= CPU_CRITICAL) {
        return 100;
    }
    mode.duty(liquid, cpu, gpu).unwrap_or(100).clamp(floor, 100)
}

fn uses_gpu(mode: &FanMode) -> bool {
    matches!(mode, FanMode::Curve { source: TempSource::Gpu | TempSource::Max, .. })
}

/// Ramp up immediately, ramp down by at most 3 % per second.
fn smooth(current: u8, target: u8) -> u8 {
    if target >= current {
        target
    } else {
        target.max(current.saturating_sub(3))
    }
}

pub fn spawn(sh: Arc<Shared>) -> Vec<JoinHandle<()>> {
    let mut v = Vec::new();
    for (name, f) in [
        ("commander", commander_thread as fn(Arc<Shared>)),
        ("board", board_thread),
        ("peripherals", peripherals_thread),
    ] {
        let sh = sh.clone();
        v.push(std::thread::Builder::new().name(name.into()).stack_size(256 * 1024).spawn(move || f(sh)).unwrap());
    }
    v
}

const CC_ID: &str = "commander_core";
const CC_NAME: &str = "Corsair iCUE Commander Core";

fn commander_thread(sh: Arc<Shared>) {
    let Ok(mut api) = hidapi::HidApi::new() else { return };
    let mut dev: Option<CommanderCore> = None;
    let mut next_open = Instant::now();
    let mut seen_rescan = 0;
    let mut gen = 0;
    let mut cfg = Config::default();
    let mut buf = Vec::new();
    let mut last_sensors = Instant::now() - Duration::from_secs(10);
    let mut duty: Option<[u8; 7]> = None;
    let mut sent = [0u8; 7];
    let mut last_duty_write = Instant::now() - Duration::from_secs(10);

    while !sh.stopped() {
        let rescan = sh.rescan_gen.load(Ordering::Relaxed);
        if rescan != seen_rescan {
            seen_rescan = rescan;
            dev = None;
            next_open = Instant::now();
        }
        if dev.is_none() {
            if Instant::now() < next_open {
                sh.nap(IDLE);
                continue;
            }
            let _ = api.refresh_devices();
            match CommanderCore::open(&api) {
                None => {
                    sh.remove_device(CC_ID);
                    next_open = Instant::now() + Duration::from_secs(10);
                }
                Some(Err(e)) => {
                    sh.set_device(info(CC_ID, CC_NAME, 0, false, e));
                    next_open = Instant::now() + Duration::from_secs(5);
                }
                Some(Ok(d)) => {
                    sh.set_device(info(CC_ID, CC_NAME, d.led_count(), true, format!("firmware {}", d.firmware)));
                    buf = vec![Rgb::BLACK; d.led_count()];
                    gen = 0;
                    sent = [0; 7];
                    dev = Some(d);
                }
            }
            continue;
        }
        let d = dev.as_mut().unwrap();
        let frame_start = Instant::now();
        let changed = match sh.config_if_changed(&mut gen) {
            Some(c) => {
                cfg = c;
                true
            }
            None => false,
        };
        let effect = cfg.effect_for(CC_ID).clone();
        let mut result = Ok(());
        if changed || effect.is_animated() || d.since_last_color() >= KEEPALIVE {
            effect.render(sh.t(), brightness(&cfg), &mut buf);
            result = d.set_colors(&buf);
        }
        if result.is_ok() && last_sensors.elapsed() >= Duration::from_secs(1) {
            last_sensors = Instant::now();
            result = d.sensors().and_then(|(rpm, liquid)| {
                let (cpu, gpu) = {
                    let mut st = sh.status.lock().unwrap();
                    st.rpm = rpm.to_vec();
                    st.liquid = liquid;
                    (st.cpu, st.gpu)
                };
                let mut next = [0u8; 7];
                for (i, slot) in next.iter_mut().enumerate() {
                    let (mode, floor) = if i == 0 { (&cfg.pump, MIN_PUMP_PERCENT) } else { (&cfg.fans, MIN_FAN_PERCENT) };
                    let target = target_duty(mode, liquid, cpu, gpu, floor);
                    *slot = duty.map_or(target, |d| smooth(d[i], target));
                }
                let duty = *duty.insert(next);
                if duty != sent || last_duty_write.elapsed() >= KEEPALIVE {
                    d.set_duty(&duty)?;
                    sent = duty;
                    last_duty_write = Instant::now();
                    sh.status.lock().unwrap().duty = duty.to_vec();
                }
                Ok(())
            });
        }
        if let Err(e) = result {
            sh.set_device(info(CC_ID, CC_NAME, 0, false, e));
            dev = None;
            next_open = Instant::now() + Duration::from_secs(2);
            continue;
        }
        let wait = if effect.is_animated() { FRAME } else { IDLE };
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
    // Dropping the device hands the hub back to its stored hardware profile.
    if let Some(d) = dev.as_mut() {
        d.restore_on_drop = !sh.handover.load(Ordering::Relaxed);
    }
    drop(dev);
}

fn board_thread(sh: Arc<Shared>) {
    let cpu = CpuTemp::open().map_err(|e| log(&format!("cpu temperature: {e}"))).ok();
    let mut gpu: Option<GpuTemp> = None;
    let mut gpu_failed_at: Option<Instant> = None;
    let bus = Smbus::open().map(Arc::new).map_err(|e| log(&format!("smbus: {e}"))).ok();
    let dimms: Vec<CorsairDram> =
        bus.as_ref().map(CorsairDram::detect).unwrap_or_default().into_iter().filter(|d| d.supports_direct()).collect();
    let ids: Vec<String> = dimms.iter().map(|d| format!("dram_{:02x}", d.addr)).collect();
    for (d, id) in dimms.iter().zip(&ids) {
        sh.set_device(info(id, "Corsair Vengeance RGB Pro", dram::LEDS, true, format!("SMBus {:#04x}", d.addr)));
    }
    let board = board_name();
    let msi = match MsiRgb::detect(&board) {
        Ok(m) => m,
        Err(e) => {
            sh.set_device(info("motherboard", &board, 1, false, e));
            None
        }
    };
    if let Some(m) = &msi {
        sh.set_device(info("motherboard", &format!("MSI {}", m.board), 1, true, "MSI-RGB Super-IO"));
    }

    let mut gen = 0;
    let mut cfg = Config::default();
    let mut last_sensors = Instant::now() - Duration::from_secs(10);
    let mut px = vec![Rgb::BLACK; dram::LEDS];
    let mut last_msi = None;
    let mut errors = vec![0u32; dimms.len()];
    let mut last_px: Vec<Option<Vec<Rgb>>> = vec![None; dimms.len()];
    let mut last_dram = Instant::now() - Duration::from_secs(1);
    while !sh.stopped() {
        let frame_start = Instant::now();
        if last_sensors.elapsed() >= Duration::from_secs(1) {
            last_sensors = Instant::now();
            let c = cpu.as_ref().and_then(CpuTemp::read);
            // NVML maps ~15 MB that never fully goes away, so only load it when a fan curve needs it.
            let want_gpu = uses_gpu(&cfg.pump) || uses_gpu(&cfg.fans);
            if want_gpu && gpu.is_none() && gpu_failed_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                match GpuTemp::open() {
                    Ok(g) => gpu = Some(g),
                    Err(e) => {
                        log(&format!("gpu temperature: {e}"));
                        gpu_failed_at = Some(Instant::now());
                    }
                }
            } else if !want_gpu {
                gpu = None;
            }
            let g = gpu.as_ref().and_then(GpuTemp::read);
            let mut st = sh.status.lock().unwrap();
            st.cpu = c;
            st.gpu = g;
        }
        let changed = match sh.config_if_changed(&mut gen) {
            Some(c) => {
                cfg = c;
                true
            }
            None => false,
        };
        let mut animated = false;
        let dram_due = changed || last_dram.elapsed() >= DRAM_FRAME;
        if dram_due {
            last_dram = Instant::now();
        }
        for (i, (d, id)) in dimms.iter().zip(&ids).enumerate() {
            let effect = cfg.effect_for(id);
            animated |= effect.is_animated();
            if !(changed || (effect.is_animated() && dram_due)) {
                continue;
            }
            effect.render(sh.t(), brightness(&cfg), &mut px);
            if !changed && last_px[i].as_deref() == Some(&px[..]) {
                continue;
            }
            match d.set_colors(&px) {
                Ok(()) => last_px[i] = Some(px.clone()),
                Err(e) => {
                    errors[i] += 1;
                    if errors[i] == 10 {
                        log(&e);
                    }
                }
            }
        }
        if let Some(m) = &msi {
            let effect = cfg.effect_for("motherboard");
            animated |= effect.is_animated();
            let mut one = [Rgb::BLACK];
            effect.render(sh.t(), brightness(&cfg), &mut one);
            // 4-bit hardware: only touch the chip when the visible color changes.
            let quant = Rgb(one[0].0 & 0xf0, one[0].1 & 0xf0, one[0].2 & 0xf0);
            if changed || last_msi != Some(quant) {
                if m.set_color(one[0]).is_ok() {
                    last_msi = Some(quant);
                }
            }
        }
        let wait = if animated { FRAME } else { IDLE };
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
}

fn peripherals_thread(sh: Arc<Shared>) {
    let Ok(mut api) = hidapi::HidApi::new() else { return };
    let mut devs: Vec<(String, CorsairV2, Instant)> = Vec::new();
    let mut next_scan = Instant::now();
    let mut seen_rescan = 0;
    let mut gen = 0;
    let mut cfg = Config::default();
    let mut buf = Vec::new();
    while !sh.stopped() {
        let rescan = sh.rescan_gen.load(Ordering::Relaxed);
        if rescan != seen_rescan || (devs.is_empty() && Instant::now() >= next_scan) {
            seen_rescan = rescan;
            for (id, _, _) in devs.drain(..) {
                sh.remove_device(&id);
            }
            let _ = api.refresh_devices();
            for (i, r) in CorsairV2::open_all(&api).into_iter().enumerate() {
                let id = format!("peripheral_{i}");
                match r {
                    Ok(d) => {
                        sh.set_device(info(&id, d.name, d.leds, true, "USB"));
                        devs.push((id, d, Instant::now() - KEEPALIVE));
                    }
                    Err(e) => sh.set_device(info(&id, "Corsair peripheral", 0, false, e)),
                }
            }
            next_scan = Instant::now() + Duration::from_secs(15);
            gen = 0;
        }
        let frame_start = Instant::now();
        let changed = match sh.config_if_changed(&mut gen) {
            Some(c) => {
                cfg = c;
                true
            }
            None => false,
        };
        let mut animated = false;
        let mut failed = None;
        for (i, (id, d, last)) in devs.iter_mut().enumerate() {
            let effect: &Effect = cfg.effect_for(id);
            animated |= effect.is_animated();
            if changed || effect.is_animated() || last.elapsed() >= KEEPALIVE {
                buf.resize(d.leds, Rgb::BLACK);
                effect.render(sh.t(), brightness(&cfg), &mut buf);
                match d.set_colors(&buf) {
                    Ok(()) => *last = Instant::now(),
                    Err(e) => failed = Some((i, e)),
                }
            }
        }
        if let Some((i, e)) = failed {
            let (id, d, _) = devs.remove(i);
            sh.set_device(info(&id, d.name, 0, false, e));
        }
        let wait = if animated { FRAME } else { IDLE };
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
    for (_, d, _) in devs.iter_mut() {
        d.restore_on_drop = !sh.handover.load(Ordering::Relaxed);
    }
}

pub fn log(msg: &str) {
    use std::io::Write;
    let path = coolrgb_core::config::config_dir().join("daemon.log");
    let _ = std::fs::create_dir_all(coolrgb_core::config::config_dir());
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duty_respects_floor_and_critical() {
        let quiet = FanMode::Fixed { percent: 10 };
        assert_eq!(target_duty(&quiet, Some(30.0), None, None, MIN_FAN_PERCENT), MIN_FAN_PERCENT);
        assert_eq!(target_duty(&quiet, Some(55.0), None, None, MIN_FAN_PERCENT), 100);
        assert_eq!(target_duty(&quiet, Some(30.0), Some(96.0), None, MIN_FAN_PERCENT), 100);
        let cpu_curve = FanMode::Curve { source: TempSource::Cpu, points: vec![(40.0, 40)] };
        assert_eq!(target_duty(&cpu_curve, Some(30.0), None, None, MIN_FAN_PERCENT), 100);
    }

    #[test]
    fn smoothing_ramps_down_slowly() {
        assert_eq!(smooth(50, 80), 80);
        assert_eq!(smooth(80, 40), 77);
    }
}
