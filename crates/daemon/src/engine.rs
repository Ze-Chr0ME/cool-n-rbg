//! Device worker threads. Each thread owns its devices, renders effects from the shared clock and
//! re-opens devices after errors (USB replug, resume from sleep).
//!
//! Driver code sits behind the manufacturer features; a build without some of them leaves shared
//! helpers unused, which is fine.
#![cfg_attr(not(all(feature = "corsair", feature = "evision", feature = "msi")), allow(dead_code, unused_imports, unused_mut, unused_variables, irrefutable_let_patterns))]
use coolrgb_core::config::{Config, FanMode, MIN_FAN_PERCENT, MIN_GPU_FAN_PERCENT, MIN_PUMP_PERCENT};
#[cfg(feature = "corsair")]
use coolrgb_core::devices::corsair::{bragi::CorsairV2, commander_core::CommanderCore, dram::{self, CorsairDram}};
#[cfg(feature = "evision")]
use coolrgb_core::devices::evision::v2::{self as evision_v2, EvisionV2};
use coolrgb_core::devices::evision::{KeyboardChange, KeyboardState};
#[cfg(feature = "msi")]
use coolrgb_core::devices::msi::superio_rgb::MsiRgb;
#[cfg(feature = "corsair")]
use coolrgb_core::devices::transport::smbus::Smbus;
use coolrgb_core::effects::{Effect, Rgb};
use coolrgb_core::ipc::{DeviceInfo, DeviceKind, Status};
use coolrgb_core::nvapi::NvGpu;
use coolrgb_core::sensors::{board_name, CpuTemp};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(50);
/// SMBus transfers busy-wait in the kernel, so DIMMs animate slower than USB devices.
#[cfg(feature = "corsair")]
const DRAM_FRAME: Duration = Duration::from_millis(100);
const IDLE: Duration = Duration::from_millis(250);
const KEEPALIVE: Duration = Duration::from_secs(4);
const LIQUID_CRITICAL: f32 = 50.0;
const CPU_CRITICAL: f32 = 95.0;
const GPU_CRITICAL: f32 = 85.0;

pub struct Shared {
    pub config: Mutex<Config>,
    pub config_gen: AtomicU64,
    pub status: Mutex<Status>,
    pub stop: AtomicBool,
    pub rescan_gen: AtomicU64,
    pub start: Instant,
    /// Set by a handover quit: leave devices in software mode for the next daemon.
    pub handover: AtomicBool,
    /// Keyboard setting changes waiting for the peripherals thread: (device id, change).
    pub keyboard_jobs: Mutex<Vec<(String, KeyboardChange)>>,
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
            keyboard_jobs: Mutex::new(Vec::new()),
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

fn info(id: &str, name: &str, kind: DeviceKind, leds: usize, ok: bool, detail: impl Into<String>) -> DeviceInfo {
    DeviceInfo { id: id.into(), name: name.into(), kind, leds, ok, detail: detail.into() }
}

fn brightness(cfg: &Config) -> f32 {
    if cfg.lights_on {
        cfg.brightness.min(100) as f32 / 100.0
    } else {
        0.0
    }
}

/// Fan/pump duty with critical-temperature override and floor. Missing sensors mean full speed.
fn target_duty(mode: &FanMode, liquid: Option<f32>, cpu: Option<f32>, gpu: Option<f32>, floor: u8) -> u8 {
    if liquid.is_some_and(|t| t >= LIQUID_CRITICAL) || cpu.is_some_and(|t| t >= CPU_CRITICAL) {
        return 100;
    }
    mode.duty(liquid, cpu, gpu).unwrap_or(100).clamp(floor, 100)
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
    let threads: &[(&str, fn(Arc<Shared>))] = &[
        #[cfg(feature = "corsair")]
        ("commander", commander_thread),
        ("board", board_thread),
        #[cfg(any(feature = "corsair", feature = "evision"))]
        ("peripherals", peripherals_thread),
    ];
    for &(name, f) in threads {
        let sh = sh.clone();
        v.push(std::thread::Builder::new().name(name.into()).stack_size(256 * 1024).spawn(move || f(sh)).unwrap());
    }
    v
}

const CC_ID: &str = "commander_core";
const CC_NAME: &str = "Corsair iCUE Commander Core";

#[cfg(feature = "corsair")]
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
    let mut led_ports = Vec::new();

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
            let ports = sh.config.lock().unwrap().led_ports.clone();
            match CommanderCore::open(&api, &ports) {
                None => {
                    sh.remove_device(CC_ID);
                    next_open = Instant::now() + Duration::from_secs(10);
                }
                Some(Err(e)) => {
                    sh.set_device(info(CC_ID, CC_NAME, DeviceKind::Controller, 0, false, e));
                    next_open = Instant::now() + Duration::from_secs(5);
                }
                Some(Ok(d)) => {
                    sh.set_device(info(CC_ID, CC_NAME, DeviceKind::Controller, d.led_count(), true, format!("firmware {}", d.firmware)));
                    {
                        let mut st = sh.status.lock().unwrap();
                        st.cc_ports_detected = d.detected.to_vec();
                        st.cc_ports = d.port_leds.to_vec();
                    }
                    led_ports = ports;
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
        // A new LED port layout needs the hub set up again.
        if cfg.led_ports != led_ports {
            // Reopen straight away; no detour through the hardware lighting profile.
            d.restore_on_drop = false;
            dev = None;
            next_open = Instant::now();
            continue;
        }
        let effects: Vec<&Effect> = (0..d.port_leds.len())
            .map(|p| if cfg.lights_on { cfg.effect_for_port(CC_ID, p) } else { &Effect::Off })
            .collect();
        let animated = effects.iter().any(|e| e.is_animated());
        let mut result = Ok(());
        if changed || animated || d.since_last_color() >= KEEPALIVE {
            render_ports(&effects, &d.port_leds, sh.t(), brightness(&cfg), &mut buf);
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
            sh.set_device(info(CC_ID, CC_NAME, DeviceKind::Controller, 0, false, e));
            dev = None;
            next_open = Instant::now() + Duration::from_secs(2);
            continue;
        }
        let wait = if animated { FRAME } else { IDLE };
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
    // Dropping the device hands the hub back to its stored hardware profile.
    if let Some(d) = dev.as_mut() {
        d.restore_on_drop = !sh.handover.load(Ordering::Relaxed);
    }
    drop(dev);
}

/// Fill `buf` port by port. Consecutive ports sharing an effect are rendered as one run, so a hub-wide
/// wave still flows across fans; a port with its own effect gets the whole effect to itself.
fn render_ports(effects: &[&Effect], port_leds: &[u16], t: f32, bright: f32, buf: &mut [Rgb]) {
    let mut start = 0;
    let mut p = 0;
    while p < effects.len() {
        let mut end = start + port_leds[p] as usize;
        let mut q = p + 1;
        while q < effects.len() && effects[q] == effects[p] {
            end += port_leds[q] as usize;
            q += 1;
        }
        let end = end.min(buf.len());
        effects[p].render(t, bright, &mut buf[start.min(end)..end]);
        start = end;
        p = q;
    }
}

fn board_thread(sh: Arc<Shared>) {
    let cpu = CpuTemp::open().map_err(|e| log(&format!("cpu temperature: {e}"))).ok();
    let mut gpu = GpuFans::new();
    #[cfg(feature = "corsair")]
    let mut dimms = Dimms::detect(&sh);
    #[cfg(feature = "msi")]
    let mut board = BoardLight::detect(&sh);

    let mut gen = 0;
    let mut cfg = Config::default();
    let mut last_sensors = Instant::now() - Duration::from_secs(10);
    while !sh.stopped() {
        let frame_start = Instant::now();
        if last_sensors.elapsed() >= Duration::from_secs(1) {
            last_sensors = Instant::now();
            let c = cpu.as_ref().and_then(CpuTemp::read);
            let liquid = {
                let mut st = sh.status.lock().unwrap();
                st.cpu = c;
                st.liquid
            };
            gpu.update(&sh, &cfg, liquid, c);
        }
        let changed = match sh.config_if_changed(&mut gen) {
            Some(c) => {
                cfg = c;
                true
            }
            None => false,
        };
        let mut animated = false;
        #[cfg(feature = "corsair")]
        {
            animated |= dimms.update(&sh, &cfg, changed);
        }
        #[cfg(feature = "msi")]
        {
            animated |= board.update(&sh, &cfg, changed);
        }
        let wait = if animated { FRAME } else { IDLE };
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
    gpu.release();
}

/// Corsair Vengeance RGB Pro DIMMs on the SMBus, with the device id each one is reported under.
#[cfg(feature = "corsair")]
struct Dimms {
    list: Vec<(CorsairDram, String)>,
    px: Vec<Rgb>,
    errors: Vec<u32>,
    last_px: Vec<Option<Vec<Rgb>>>,
    last_frame: Instant,
}

#[cfg(feature = "corsair")]
impl Dimms {
    fn detect(sh: &Shared) -> Dimms {
        let bus = Smbus::open().map(Arc::new).map_err(|e| log(&format!("smbus: {e}"))).ok();
        let dimms: Vec<CorsairDram> =
            bus.as_ref().map(CorsairDram::detect).unwrap_or_default().into_iter().filter(|d| d.supports_direct()).collect();
        let mut list = Vec::new();
        for (n, d) in dimms.into_iter().enumerate() {
            let id = format!("dram_{:02x}", d.addr);
            let name = format!("Corsair Vengeance RGB Pro #{}", n + 1);
            sh.set_device(info(&id, &name, DeviceKind::Memory, dram::LEDS, true, format!("SMBus {:#04x}", d.addr)));
            list.push((d, id));
        }
        let n = list.len();
        Dimms {
            list,
            px: vec![Rgb::BLACK; dram::LEDS],
            errors: vec![0; n],
            last_px: vec![None; n],
            last_frame: Instant::now() - Duration::from_secs(1),
        }
    }

    /// Render one frame where due; true when any DIMM runs an animated effect.
    fn update(&mut self, sh: &Shared, cfg: &Config, changed: bool) -> bool {
        let mut animated = false;
        let due = changed || self.last_frame.elapsed() >= DRAM_FRAME;
        if due {
            self.last_frame = Instant::now();
        }
        for (i, (d, id)) in self.list.iter().enumerate() {
            let effect = if cfg.lights_on { cfg.effect_for(id) } else { &Effect::Off };
            animated |= effect.is_animated();
            if !(changed || (effect.is_animated() && due)) {
                continue;
            }
            effect.render(sh.t(), brightness(cfg), &mut self.px);
            if !changed && self.last_px[i].as_deref() == Some(&self.px[..]) {
                continue;
            }
            match d.set_colors(&self.px) {
                Ok(()) => self.last_px[i] = Some(self.px.clone()),
                Err(e) => {
                    self.errors[i] += 1;
                    if self.errors[i] == 10 {
                        log(&e);
                    }
                }
            }
        }
        animated
    }
}

/// MSI-RGB onboard lighting, reported as the "motherboard" device.
#[cfg(feature = "msi")]
struct BoardLight {
    msi: Option<MsiRgb>,
    last: Option<Rgb>,
}

#[cfg(feature = "msi")]
impl BoardLight {
    fn detect(sh: &Shared) -> BoardLight {
        let board = board_name();
        let msi = match MsiRgb::detect(&board) {
            Ok(m) => m,
            Err(e) => {
                sh.set_device(info("motherboard", &board, DeviceKind::Motherboard, 1, false, e));
                None
            }
        };
        if let Some(m) = &msi {
            sh.set_device(info("motherboard", &format!("MSI {}", m.board), DeviceKind::Motherboard, 1, true, "MSI-RGB Super-IO"));
        }
        BoardLight { msi, last: None }
    }

    /// True when the effect is animated.
    fn update(&mut self, sh: &Shared, cfg: &Config, changed: bool) -> bool {
        let Some(m) = &self.msi else { return false };
        let effect = if cfg.lights_on { cfg.effect_for("motherboard") } else { &Effect::Off };
        let mut one = [Rgb::BLACK];
        effect.render(sh.t(), brightness(cfg), &mut one);
        // 4-bit hardware: only touch the chip when the visible color changes.
        let quant = Rgb(one[0].0 & 0xf0, one[0].1 & 0xf0, one[0].2 & 0xf0);
        if changed || self.last != Some(quant) {
            if m.set_color(one[0]).is_ok() {
                self.last = Some(quant);
            }
        }
        effect.is_animated()
    }
}

/// Marker meaning "we left the GPU fans in manual mode"; lets a restart after a crash hand them back
/// to the card without overriding other tools (e.g. MSI Afterburner) when we never touched them.
fn gpu_marker() -> std::path::PathBuf {
    coolrgb_core::config::config_dir().join("gpu-fans-manual")
}

/// Hand GPU fans back to the card's automatic curve. Also called from the panic hook.
pub fn restore_gpu_fans() {
    if gpu_marker().exists() {
        if let Ok(g) = NvGpu::open() {
            if g.set_all(None).is_ok() {
                let _ = std::fs::remove_file(gpu_marker());
            }
        }
    }
}

/// How long a GPU curve must ask for 0 % before the fans are handed to the card's fan-stop.
const GPU_STOP_DELAY: Duration = Duration::from_secs(20);

struct GpuFans {
    nv: Option<NvGpu>,
    duty: Option<u8>,
    manual: bool,
    last_write: Instant,
    /// Since when the curve has been asking for 0 %.
    zero_since: Option<Instant>,
}

impl GpuFans {
    fn new() -> GpuFans {
        restore_gpu_fans();
        let nv = NvGpu::open().map_err(|e| log(&format!("nvapi: {e}"))).ok();
        GpuFans { nv, duty: None, manual: false, last_write: Instant::now(), zero_since: None }
    }

    fn update(&mut self, sh: &Shared, cfg: &Config, liquid: Option<f32>, cpu: Option<f32>) {
        let Some(nv) = self.nv.take() else { return };
        self.update_with(&nv, sh, cfg, liquid, cpu);
        self.nv = Some(nv);
    }

    fn update_with(&mut self, nv: &NvGpu, sh: &Shared, cfg: &Config, liquid: Option<f32>, cpu: Option<f32>) {
        let temp = nv.temperature();
        // A curve point at 0 % means "let the card stop its fans": NVIDIA's manual mode can't go
        // below 30 %, but the card's own automatic mode has zero-RPM.
        let raw = match &cfg.gpu_fans {
            None => None,
            Some(_) if temp.is_some_and(|t| t >= GPU_CRITICAL) => Some(100),
            Some(mode) => Some(mode.duty(liquid, cpu, temp).unwrap_or(100)),
        };
        if raw == Some(0) {
            self.zero_since.get_or_insert_with(Instant::now);
        } else {
            self.zero_since = None;
        }
        let stop = self.zero_since.is_some_and(|t| t.elapsed() >= GPU_STOP_DELAY) || (raw == Some(0) && !self.manual);
        match &cfg.gpu_fans {
            None => self.to_auto(nv),
            Some(_) if stop => self.to_auto(nv),
            Some(_) => {
                // While waiting out GPU_STOP_DELAY keep the fans at the lowest manual speed.
                let target = raw.unwrap_or(100).clamp(MIN_GPU_FAN_PERCENT, 100);
                let d = self.duty.map_or(target, |cur| smooth(cur, target));
                if !self.manual || Some(d) != self.duty || self.last_write.elapsed() >= Duration::from_secs(10) {
                    if !self.manual {
                        let _ = std::fs::create_dir_all(coolrgb_core::config::config_dir());
                        let _ = std::fs::write(gpu_marker(), b"1");
                    }
                    match nv.set_all(Some(d as u32)) {
                        Ok(()) => {
                            self.manual = true;
                            self.last_write = Instant::now();
                        }
                        Err(e) => log(&e),
                    }
                }
                self.duty = Some(d);
            }
        }
        let coolers = nv.coolers().unwrap_or_default();
        let mut st = sh.status.lock().unwrap();
        st.gpu = temp;
        st.gpu_fan_rpm = coolers.iter().map(|c| c.rpm).collect();
        st.gpu_fan_percent = coolers.iter().map(|c| c.percent).collect();
        st.gpu_fan_manual = coolers.iter().any(|c| c.manual);
    }

    fn to_auto(&mut self, nv: &NvGpu) {
        if self.manual && nv.set_all(None).is_ok() {
            self.manual = false;
            self.duty = None;
            let _ = std::fs::remove_file(gpu_marker());
        }
    }

    fn release(&mut self) {
        if self.manual {
            if let Some(nv) = &self.nv {
                if nv.set_all(None).is_ok() {
                    let _ = std::fs::remove_file(gpu_marker());
                }
            }
        }
    }
}

/// USB lighting devices handled by the peripherals thread.
#[cfg(any(feature = "corsair", feature = "evision"))]
enum Peripheral {
    #[cfg(feature = "corsair")]
    Corsair(CorsairV2),
    #[cfg(feature = "evision")]
    Keyboard(EvisionV2),
}

#[cfg(any(feature = "corsair", feature = "evision"))]
impl Peripheral {
    fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(d) => d.name,
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(d) => d.name,
        }
    }

    /// How often colors must be re-sent even when nothing changed.
    fn keepalive(&self) -> Duration {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(_) => KEEPALIVE,
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(_) => evision_v2::KEEPALIVE,
        }
    }

    fn render(&mut self, effect: &Effect, t: f32, bright: f32, buf: &mut Vec<Rgb>) -> Result<(), String> {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(d) => {
                buf.resize(d.leds, Rgb::BLACK);
                effect.render(t, bright, buf);
                d.set_colors(buf)
            }
            #[cfg(feature = "evision")]
            // Effects run across the keyboard's columns, so waves sweep left to right.
            Peripheral::Keyboard(d) => {
                buf.resize(d.columns(), Rgb::BLACK);
                effect.render(t, bright, buf);
                d.set_columns(buf)
            }
        }
    }

    fn kind(&self) -> DeviceKind {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(d) => d.kind,
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(_) => DeviceKind::Keyboard,
        }
    }

    /// A keyboard running its built-in effect needs a full frame to come back to direct mode.
    fn leaving_onboard(&self) -> bool {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(_) => false,
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(k) => !k.direct,
        }
    }

    /// Keepalive write when nothing changed.
    fn refresh(&mut self, effect: &Effect, t: f32, bright: f32, buf: &mut Vec<Rgb>) -> Result<(), String> {
        match self {
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(k) => k.refresh(),
            #[allow(unreachable_patterns)]
            _ => self.render(effect, t, bright, buf),
        }
    }

    fn set_restore(&mut self, restore: bool) {
        match self {
            #[cfg(feature = "corsair")]
            Peripheral::Corsair(d) => d.restore_on_drop = restore,
            #[cfg(feature = "evision")]
            Peripheral::Keyboard(d) => d.restore_on_drop = restore,
        }
    }
}

#[cfg(feature = "evision")]
fn set_keyboard_state(sh: &Shared, state: KeyboardState) {
    let mut st = sh.status.lock().unwrap();
    st.keyboards.retain(|k| k.id != state.id);
    st.keyboards.push(state);
}

#[cfg(any(feature = "corsair", feature = "evision"))]
fn peripherals_thread(sh: Arc<Shared>) {
    let Ok(mut api) = hidapi::HidApi::new() else { return };
    let mut devs: Vec<(String, Peripheral, Instant)> = Vec::new();
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
            sh.status.lock().unwrap().keyboards.clear();
            let _ = api.refresh_devices();
            #[cfg(feature = "corsair")]
            for (i, r) in CorsairV2::open_all(&api).into_iter().enumerate() {
                let id = format!("peripheral_{i}");
                match r {
                    Ok(d) => {
                        sh.set_device(info(&id, d.name, d.kind, d.leds, true, "USB"));
                        devs.push((id, Peripheral::Corsair(d), Instant::now() - KEEPALIVE));
                    }
                    Err(e) => sh.set_device(info(&id, "Corsair peripheral", DeviceKind::Other, 0, false, e)),
                }
            }
            #[cfg(feature = "evision")]
            for (i, r) in EvisionV2::open_all(&api).into_iter().enumerate() {
                let id = format!("keyboard_{i}");
                match r {
                    Ok(d) => {
                        sh.set_device(info(&id, d.name, DeviceKind::Keyboard, d.slots, true, "USB · EVision"));
                        set_keyboard_state(&sh, d.state(&id));
                        devs.push((id, Peripheral::Keyboard(d), Instant::now() - KEEPALIVE));
                    }
                    Err(e) => sh.set_device(info(&id, "EVision keyboard", DeviceKind::Keyboard, 0, false, e)),
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
        // Settings changes from the control panel.
        #[cfg(feature = "evision")]
        for (job_id, change) in std::mem::take(&mut *sh.keyboard_jobs.lock().unwrap()) {
            if let Some((id, Peripheral::Keyboard(k), _)) = devs.iter_mut().find(|(id, _, _)| *id == job_id) {
                let mut state = k.state(id);
                if let Err(e) = k.apply(&change).and_then(|()| {
                    // A new profile may hold another built-in effect; show it right away.
                    if cfg.lights_on && cfg.onboard_lighting.contains(id.as_str()) && k.direct {
                        k.hardware_mode()?;
                    }
                    Ok(())
                }) {
                    log(&e);
                    state.error = Some(e);
                } else {
                    state = k.state(id);
                }
                set_keyboard_state(&sh, state);
            }
        }
        let mut wait = IDLE;
        let mut failed = None;
        for (i, (id, d, last)) in devs.iter_mut().enumerate() {
            // A keyboard set to its built-in effect is left alone, except to turn it dark with the master switch.
            #[cfg(feature = "evision")]
            if let Peripheral::Keyboard(k) = d {
                if cfg.lights_on && cfg.onboard_lighting.contains(id.as_str()) {
                    if k.direct {
                        if let Err(e) = k.hardware_mode() {
                            failed = Some((i, e));
                        }
                    }
                    continue;
                }
            }
            let effect: &Effect = if cfg.lights_on { cfg.effect_for(id) } else { &Effect::Off };
            if effect.is_animated() {
                wait = FRAME;
            }
            let result = if changed || effect.is_animated() || d.leaving_onboard() {
                Some(d.render(effect, sh.t(), brightness(&cfg), &mut buf))
            } else if last.elapsed() >= d.keepalive() {
                Some(d.refresh(effect, sh.t(), brightness(&cfg), &mut buf))
            } else {
                None
            };
            match result {
                Some(Ok(())) => *last = Instant::now(),
                Some(Err(e)) => failed = Some((i, e)),
                None => {}
            }
            // Wake up in time for the next keepalive.
            wait = wait.min(d.keepalive().saturating_sub(last.elapsed()));
        }
        if let Some((i, e)) = failed {
            let (id, d, _) = devs.remove(i);
            sh.set_device(info(&id, d.name(), d.kind(), 0, false, e));
            sh.status.lock().unwrap().keyboards.retain(|k| k.id != id);
        }
        sh.nap(wait.saturating_sub(frame_start.elapsed()));
    }
    for (_, d, _) in devs.iter_mut() {
        d.set_restore(!sh.handover.load(Ordering::Relaxed));
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
    use coolrgb_core::config::TempSource;

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
    fn ports_render_their_own_effects() {
        let red = Effect::Static { color: Rgb(255, 0, 0) };
        let effects = [&Effect::Off, &red, &red, &Effect::Off];
        let mut buf = vec![Rgb(1, 1, 1); 7];
        render_ports(&effects, &[1, 2, 0, 4], 0.0, 1.0, &mut buf);
        assert_eq!(buf[0], Rgb::BLACK);
        assert_eq!(&buf[1..3], &[Rgb(255, 0, 0); 2]);
        assert_eq!(&buf[3..], &[Rgb::BLACK; 4]);
    }

    #[test]
    fn smoothing_ramps_down_slowly() {
        assert_eq!(smooth(50, 80), 80);
        assert_eq!(smooth(80, 40), 77);
    }
}
