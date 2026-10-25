//! coolrgb: control panel. Talks to the background daemon (coolrgbd) over a named pipe; closing this
//! window frees all of its memory while lighting and fan control keep running in the daemon.
#![windows_subsystem = "windows"]

mod fan_curve;

use coolrgb_core::config::{default_gpu_curve, port_key, presets, Config, FanMode, TempSource, MIN_FAN_PERCENT, MIN_GPU_FAN_PERCENT, MIN_PUMP_PERCENT};
use coolrgb_core::effects::{Effect, Rgb};
use coolrgb_core::devices::evision_v2::{KeyboardChange, KeyboardState, OnboardLighting, DEBOUNCE_MS, MAX_BRIGHTNESS, MODES, POLLING_HZ, PROFILES, SLOWEST};
use coolrgb_core::ipc::{call, DeviceInfo, DeviceKind, Request, Response, Status};
use eframe::egui::{self, Color32, RichText};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Link {
    status: Option<Status>,
    config: Option<Config>,
    error: Option<String>,
}

enum Msg {
    SetConfig(Config),
    Keyboard { id: String, change: KeyboardChange },
    Rescan,
    Quit,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Lighting,
    Fans,
    Devices,
    Settings,
}

struct App {
    link: Arc<Mutex<Link>>,
    tx: Sender<Msg>,
    cfg: Option<Config>,
    dirty: bool,
    last_send: Instant,
    last_edit: Instant,
    tab: Tab,
    /// None = edit the global effect, Some(id) = a device override.
    target: Option<String>,
    /// With the Commander Core targeted: None = whole hub, Some(p) = one LED port.
    port: Option<usize>,
    started_daemon: bool,
    /// Device type selected on the Devices tab.
    dev_kind: DeviceKind,
    /// Keyboard built-in effect being edited (device id, values) and whether it still has to be sent.
    kb_edit: Option<(String, OnboardLighting)>,
    kb_dirty: bool,
    kb_last_edit: Instant,
}

/// Background I/O so the UI never blocks on the pipe.
fn worker(link: Arc<Mutex<Link>>, rx: Receiver<Msg>, ctx: egui::Context) {
    loop {
        let mut msgs = Vec::new();
        match rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(m) => msgs.push(m),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        msgs.extend(rx.try_iter());
        // Only the newest config matters.
        let mut latest_cfg = None;
        for m in msgs {
            match m {
                Msg::SetConfig(c) => latest_cfg = Some(c),
                Msg::Rescan => drop(call(&Request::Rescan)),
                Msg::Quit => drop(call(&Request::Quit)),
                Msg::Keyboard { id, change } => drop(call(&Request::Keyboard { id, change })),
            }
        }
        if let Some(c) = latest_cfg {
            let _ = call(&Request::SetConfig { config: c });
        }
        // Always refresh: the tray menu can change the config while this window is open.
        let cfg = match call(&Request::GetConfig) {
            Ok(Response::Config { config }) => Some(config),
            _ => None,
        };
        let st = call(&Request::Status);
        {
            let mut l = link.lock().unwrap();
            match st {
                Ok(Response::Status { status }) => {
                    l.status = Some(status);
                    l.error = None;
                }
                Ok(_) => l.error = Some("unexpected reply".into()),
                Err(e) => {
                    l.status = None;
                    l.config = None;
                    l.error = Some(e);
                }
            }
            if cfg.is_some() {
                l.config = cfg;
            }
        }
        ctx.request_repaint();
    }
}

fn start_daemon() {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let Ok(exe) = std::env::current_exe() else { return };
    let path: Vec<u16> = exe.with_file_name("coolrgbd.exe").as_os_str().encode_wide_z();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), path.as_ptr(), std::ptr::null(), std::ptr::null(), SW_HIDE) };
}

trait EncodeWideZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}

impl EncodeWideZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

fn rgb32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

const PRESETS: [Rgb; 8] = [
    Rgb(255, 255, 255),
    Rgb(255, 0, 0),
    Rgb(255, 120, 0),
    Rgb(255, 230, 0),
    Rgb(0, 255, 60),
    Rgb(0, 160, 255),
    Rgb(40, 0, 255),
    Rgb(200, 0, 255),
];

/// Effect editor. Returns true when something changed.
fn effect_ui(ui: &mut egui::Ui, e: &mut Effect) -> bool {
    let mut changed = false;
    let color = match e {
        Effect::Static { color } | Effect::Breathing { color, .. } => *color,
        _ => Rgb(0, 160, 255),
    };
    let period = match e {
        Effect::Breathing { period_s, .. } | Effect::Rainbow { period_s } | Effect::Wave { period_s } => *period_s,
        _ => 4.0,
    };
    ui.horizontal(|ui| {
        for (label, new) in [
            ("Off", Effect::Off),
            ("Static", Effect::Static { color }),
            ("Breathing", Effect::Breathing { color, period_s: period }),
            ("Rainbow", Effect::Rainbow { period_s: period }),
            ("Wave", Effect::Wave { period_s: period }),
        ] {
            let selected = std::mem::discriminant(e) == std::mem::discriminant(&new);
            if ui.selectable_label(selected, label).clicked() && !selected {
                *e = new;
                changed = true;
            }
        }
    });
    ui.add_space(8.0);
    if let Effect::Static { color } | Effect::Breathing { color, .. } = e {
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut c = [color.0, color.1, color.2];
            if ui.color_edit_button_srgb(&mut c).changed() {
                *color = Rgb(c[0], c[1], c[2]);
                changed = true;
            }
            for p in PRESETS {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 4.0, rgb32(p));
                if *color == p {
                    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
                }
                if resp.clicked() {
                    *color = p;
                    changed = true;
                }
            }
        });
    }
    if let Effect::Breathing { period_s, .. } | Effect::Rainbow { period_s } | Effect::Wave { period_s } = e {
        ui.horizontal(|ui| {
            ui.label("Cycle time");
            changed |= ui.add(egui::Slider::new(period_s, 0.5..=30.0).suffix(" s").logarithmic(true)).changed();
        });
    }
    changed
}

fn preview(ui: &mut egui::Ui, e: &Effect, brightness: f32) {
    let mut px = vec![Rgb::BLACK; 32];
    let t = ui.input(|i| i.time) as f32;
    e.render(t, brightness, &mut px);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(480.0), 18.0), egui::Sense::hover());
    let w = rect.width() / px.len() as f32;
    for (i, c) in px.iter().enumerate() {
        let r = egui::Rect::from_min_size(rect.min + egui::vec2(i as f32 * w, 0.0), egui::vec2(w + 0.5, rect.height()));
        ui.painter().rect_filled(r, 0.0, rgb32(*c));
    }
    if e.is_animated() {
        ui.ctx().request_repaint_after(Duration::from_millis(40));
    }
}

fn fmt_temp(t: Option<f32>) -> String {
    t.map_or("–".into(), |t| format!("{t:.1} °C"))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = Color32::from_rgb(0, 110, 190);
        cc.egui_ctx.set_visuals(visuals);
        let link = Arc::new(Mutex::new(Link::default()));
        let (tx, rx) = mpsc::channel();
        let (l, ctx) = (link.clone(), cc.egui_ctx.clone());
        std::thread::spawn(move || worker(l, rx, ctx));
        App {
            link,
            tx,
            cfg: None,
            dirty: false,
            last_send: Instant::now(),
            last_edit: Instant::now(),
            tab: match arg_value("--tab").as_deref() {
                Some("fans") => Tab::Fans,
                Some("devices") => Tab::Devices,
                Some("settings") => Tab::Settings,
                _ => Tab::Lighting,
            },
            target: None,
            port: None,
            started_daemon: false,
            dev_kind: DeviceKind::Keyboard,
            kb_edit: None,
            kb_dirty: false,
            kb_last_edit: Instant::now(),
        }
    }

    fn lighting(&mut self, ui: &mut egui::Ui, status: &Status) {
        let cfg = self.cfg.as_mut().unwrap();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Brightness");
            changed |= ui.add(egui::Slider::new(&mut cfg.brightness, 0..=100).suffix(" %")).changed();
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Apply to");
            let current = match &self.target {
                None => "All devices".to_string(),
                Some(id) => status.devices.iter().find(|d| &d.id == id).map_or(id.clone(), |d| d.name.clone()),
            };
            egui::ComboBox::from_id_salt("target").selected_text(current).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.target, None, "All devices");
                for d in &status.devices {
                    let prefix = format!("{}:", d.id);
                    let custom = cfg.per_device.keys().any(|k| k == &d.id || k.starts_with(&prefix));
                    let label = if custom { format!("{} (custom)", d.name) } else { d.name.clone() };
                    if ui.selectable_value(&mut self.target, Some(d.id.clone()), label).changed() {
                        self.port = None;
                    }
                }
            });
        });
        let hub = self.target.as_deref() == Some(CC_ID) && !status.cc_ports.is_empty();
        if hub {
            ui.horizontal(|ui| {
                ui.label("Port");
                let name = |p: usize| port_label(p, status.cc_ports[p]);
                let current = self.port.map_or("Whole hub".to_string(), name);
                egui::ComboBox::from_id_salt("port").selected_text(current).width(260.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.port, None, "Whole hub");
                    for p in (0..status.cc_ports.len()).filter(|&p| status.cc_ports[p] > 0) {
                        let label = if cfg.per_device.contains_key(&port_key(CC_ID, p)) { format!("{} (custom)", name(p)) } else { name(p) };
                        ui.selectable_value(&mut self.port, Some(p), label);
                    }
                });
            });
        } else {
            self.port = None;
        }
        ui.separator();
        if !cfg.lights_on {
            ui.label(RichText::new("Lights are off. The preview shows what they will look like when switched on.").color(Color32::from_rgb(230, 170, 80)));
            ui.add_space(4.0);
        }
        let bright = cfg.brightness as f32 / 100.0;
        match self.target.clone() {
            None => {
                changed |= effect_ui(ui, &mut cfg.effect);
                ui.add_space(8.0);
                preview(ui, &cfg.effect, bright);
            }
            Some(id) => {
                // A port inherits from its hub, the hub from the global effect.
                let (key, what, follows) = match self.port {
                    Some(p) => (port_key(&id, p), "port", "Follows the effect set for the whole hub."),
                    None => (id.clone(), "device", "Follows the effect set for all devices."),
                };
                let mut custom = cfg.per_device.contains_key(&key);
                if ui.checkbox(&mut custom, format!("Use a custom effect for this {what}")).changed() {
                    if custom {
                        let start = cfg.effect_for(&id).clone();
                        cfg.per_device.insert(key.clone(), start);
                    } else {
                        cfg.per_device.remove(&key);
                    }
                    changed = true;
                }
                if let Some(e) = cfg.per_device.get_mut(&key) {
                    changed |= effect_ui(ui, e);
                    ui.add_space(8.0);
                    preview(ui, e, bright);
                } else {
                    ui.label(RichText::new(follows).weak());
                }
                if hub && self.port.is_none() {
                    let own: Vec<String> = (0..status.cc_ports.len())
                        .filter(|&p| status.cc_ports[p] > 0 && cfg.per_device.contains_key(&port_key(&id, p)))
                        .map(|p| port_label(p, status.cc_ports[p]))
                        .collect();
                    if !own.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("Own effect, not changed here: {}", own.join(", "))).weak());
                    }
                }
            }
        }
        self.dirty |= changed;
    }

    fn fans(&mut self, ui: &mut egui::Ui, st: &Status) {
        ui.horizontal(|ui| {
            for (label, t) in [("Liquid", st.liquid), ("CPU", st.cpu), ("GPU", st.gpu)] {
                ui.label(RichText::new(label).weak());
                ui.label(RichText::new(fmt_temp(t)).strong());
                ui.add_space(12.0);
            }
        });
        if !st.rpm.is_empty() {
            ui.add_space(4.0);
            egui::Grid::new("rpm").spacing([18.0, 2.0]).show(ui, |ui| {
                for i in 0..st.rpm.len() {
                    ui.label(RichText::new(if i == 0 { "Pump".to_string() } else { format!("Fan {i}") }).weak());
                }
                ui.end_row();
                for rpm in &st.rpm {
                    ui.label(RichText::new(format!("{rpm} rpm")).strong());
                }
                ui.end_row();
                for d in &st.duty {
                    ui.label(format!("{d} %"));
                }
                ui.end_row();
            });
        }
        ui.separator();
        let cfg = self.cfg.as_mut().unwrap();
        let temps = (st.liquid, st.cpu, st.gpu);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Presets");
            for p in presets() {
                let active = cfg.pump == p.pump && cfg.fans == p.fans && cfg.gpu_fans == p.gpu_fans;
                if ui.selectable_label(active, p.name).clicked() && !active {
                    cfg.pump = p.pump;
                    cfg.fans = p.fans;
                    cfg.gpu_fans = p.gpu_fans;
                    changed = true;
                }
            }
            ui.label(RichText::new("applies to pump, case fans and graphics card").weak());
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Pump");
            changed |= fan_mode_ui(ui, "pump", &mut cfg.pump, MIN_PUMP_PERCENT, temps);
            ui.add_space(14.0);
            ui.heading("Fans (all 6)");
            changed |= fan_mode_ui(ui, "fans", &mut cfg.fans, MIN_FAN_PERCENT, temps);
            ui.add_space(14.0);
            ui.heading("Graphics card fans");
            if !st.gpu_fan_rpm.is_empty() {
                let fans: Vec<String> = st
                    .gpu_fan_rpm
                    .iter()
                    .zip(&st.gpu_fan_percent)
                    .map(|(r, p)| format!("{r} rpm · {p} %"))
                    .collect();
                ui.label(fans.join("    "));
            }
            changed |= gpu_fans_ui(ui, &mut cfg.gpu_fans, temps);
        });
        self.dirty |= changed;
    }
}

impl App {
    /// Devices tab: a list of device types on the left, that type's devices and settings on the right.
    fn devices(&mut self, ui: &mut egui::Ui, st: &Status) {
        let mut kinds: Vec<DeviceKind> = st.devices.iter().map(|d| d.kind).collect();
        kinds.sort();
        kinds.dedup();
        if !kinds.contains(&self.dev_kind) {
            self.dev_kind = kinds.first().copied().unwrap_or_default();
        }
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(120.0);
                for k in &kinds {
                    if ui.selectable_label(self.dev_kind == *k, RichText::new(k.label()).size(15.0)).clicked() {
                        self.dev_kind = *k;
                    }
                    ui.add_space(2.0);
                }
                ui.add_space(12.0);
                if ui.button("Rescan devices").clicked() {
                    let _ = self.tx.send(Msg::Rescan);
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.device_page(ui, st));
            });
        });
    }

    fn device_page(&mut self, ui: &mut egui::Ui, st: &Status) {
        let devs: Vec<&DeviceInfo> = st.devices.iter().filter(|d| d.kind == self.dev_kind).collect();
        egui::Grid::new("devs").striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
            for h in ["Device", "LEDs", "State", "Details"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for d in &devs {
                ui.label(&d.name);
                ui.label(d.leds.to_string());
                if d.ok {
                    ui.label(RichText::new("OK").color(Color32::from_rgb(90, 200, 120)));
                } else {
                    ui.label(RichText::new("Error").color(Color32::from_rgb(230, 90, 80)));
                }
                ui.label(RichText::new(&d.detail).weak());
                ui.end_row();
            }
        });
        ui.add_space(12.0);
        match self.dev_kind {
            DeviceKind::Keyboard => {
                for kb in &st.keyboards {
                    let name = devs.iter().find(|d| d.id == kb.id).map_or(kb.id.as_str(), |d| d.name.as_str());
                    self.keyboard_ui(ui, kb, name);
                }
                ui.add_space(10.0);
                if ui.button("Windows keyboard settings…").on_hover_text("Key repeat delay and rate").clicked() {
                    let _ = std::process::Command::new("control").arg("keyboard").spawn();
                }
            }
            DeviceKind::Controller if !st.cc_ports.is_empty() => {
                ui.heading("Commander Core LED ports");
                ui.label(
                    RichText::new("Pick the fan type on a port if its lights stay dark or show mixed colors (the hub sometimes misdetects them).")
                        .weak(),
                );
                let cfg = self.cfg.as_mut().unwrap();
                if led_ports_ui(ui, &mut cfg.led_ports, &st.cc_ports_detected, &st.cc_ports) {
                    self.dirty = true;
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Per-port effects are on the Lighting tab (Apply to: Commander Core, then pick a port).").weak());
            }
            _ => {
                ui.label(RichText::new("Lighting for these devices is set on the Lighting tab. No other settings yet.").weak());
            }
        }
    }

    fn keyboard_ui(&mut self, ui: &mut egui::Ui, kb: &KeyboardState, name: &str) {
        ui.heading(name);
        if let Some(e) = &kb.error {
            ui.label(RichText::new(e).color(Color32::from_rgb(230, 90, 80)));
        }
        let cfg = self.cfg.as_mut().unwrap();
        ui.horizontal(|ui| {
            ui.label("Lighting");
            let onboard = cfg.onboard_lighting.contains(&kb.id);
            if ui.selectable_label(!onboard, "Cool'n RGB effects").clicked() && onboard {
                cfg.onboard_lighting.remove(&kb.id);
                self.dirty = true;
            }
            if ui.selectable_label(onboard, "Built-in effect").clicked() && !onboard {
                cfg.onboard_lighting.insert(kb.id.clone());
                self.dirty = true;
            }
        });
        ui.label(
            RichText::new("The built-in effect runs on the keyboard itself. It also shows whenever Cool'n RGB isn't running. \"Lights: off\" still turns the keyboard dark.")
                .weak(),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Onboard profile");
            for p in 0..PROFILES {
                if ui.selectable_label(kb.profile == p, format!("{}", p + 1)).clicked() && kb.profile != p {
                    self.kb_edit = None;
                    let _ = self.tx.send(Msg::Keyboard { id: kb.id.clone(), change: KeyboardChange::Profile { profile: p } });
                }
            }
        });
        ui.label(RichText::new("Each profile stores its own built-in effect, debounce, polling rate (and key settings made in the stock app).").weak());
        ui.add_space(8.0);
        egui::Grid::new(("kb-timing", &kb.id)).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label("Debounce");
            ui.horizontal(|ui| {
                for ms in DEBOUNCE_MS.iter().rev() {
                    if ui.selectable_label(kb.debounce_ms == Some(*ms), format!("{ms} ms")).clicked() && kb.debounce_ms != Some(*ms) {
                        let _ = self.tx.send(Msg::Keyboard { id: kb.id.clone(), change: KeyboardChange::Debounce { ms: *ms } });
                    }
                }
            });
            ui.end_row();
            ui.label("USB polling rate");
            ui.horizontal(|ui| {
                for hz in POLLING_HZ {
                    if ui.selectable_label(kb.polling_hz == Some(hz), format!("{hz} Hz")).clicked() && kb.polling_hz != Some(hz) {
                        let _ = self.tx.send(Msg::Keyboard { id: kb.id.clone(), change: KeyboardChange::PollingRate { hz } });
                    }
                }
            });
            ui.end_row();
        });
        ui.label(RichText::new("Lower debounce reacts faster; raise it if a key sometimes types twice.").weak());
        ui.add_space(8.0);
        let Some(stored) = kb.lighting else { return };
        // Edits stay local until the user pauses, so dragging a slider doesn't rewrite the keyboard's memory each frame.
        let mut l = match &self.kb_edit {
            Some((id, l)) if *id == kb.id => *l,
            _ => stored,
        };
        let before = l;
        ui.label(RichText::new("Built-in effect").strong());
        egui::Grid::new(("kb-onboard", &kb.id)).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label("Effect");
            let current = MODES.iter().find(|m| m.0 == l.mode).map_or(format!("{:#04x}", l.mode), |m| m.1.to_string());
            egui::ComboBox::from_id_salt(("kb-mode", &kb.id)).selected_text(current).width(300.0).show_ui(ui, |ui| {
                for (code, label) in MODES.iter().filter(|m| kb.modes.contains(&m.0)) {
                    ui.selectable_value(&mut l.mode, *code, *label);
                }
            });
            ui.end_row();
            ui.label("Brightness");
            ui.add(egui::Slider::new(&mut l.brightness, 0..=MAX_BRIGHTNESS).custom_formatter(|v, _| {
                if v == 0.0 { "off".into() } else { format!("{v}") }
            }));
            ui.end_row();
            ui.label("Speed");
            let mut fast = SLOWEST - l.speed.min(SLOWEST);
            if ui.add(egui::Slider::new(&mut fast, 0..=SLOWEST).custom_formatter(|v, _| match v as u8 {
                0 => "slowest".into(),
                SLOWEST => "fastest".into(),
                v => format!("{v}"),
            })).changed() {
                l.speed = SLOWEST - fast;
            }
            ui.end_row();
            ui.label("Direction");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut l.direction, 1, "Clockwise / right");
                ui.selectable_value(&mut l.direction, 0, "Counter-clockwise / left");
            });
            ui.end_row();
            ui.label("Color");
            ui.horizontal(|ui| {
                ui.checkbox(&mut l.random_colors, "Random colors");
                ui.add_enabled_ui(!l.random_colors, |ui| {
                    let mut c = [l.color.0, l.color.1, l.color.2];
                    if ui.color_edit_button_srgb(&mut c).changed() {
                        l.color = Rgb(c[0], c[1], c[2]);
                    }
                    for p in PRESETS {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                        ui.painter().rect_filled(rect, 4.0, rgb32(p));
                        if resp.clicked() {
                            l.color = p;
                        }
                    }
                });
            });
            ui.end_row();
        });
        if l != before {
            self.kb_edit = Some((kb.id.clone(), l));
            self.kb_last_edit = Instant::now();
            self.kb_dirty = true;
        }
        let pending = self.kb_dirty || matches!(&self.kb_edit, Some((id, e)) if *id == kb.id && *e != stored);
        if pending {
            ui.label(RichText::new("Saving to the keyboard…").weak());
        }
    }

    /// Send a paused keyboard edit; drop the local copy once the keyboard reports it back.
    fn flush_keyboard_edit(&mut self, ctx: &egui::Context, st: Option<&Status>) {
        let Some((id, l)) = self.kb_edit.clone() else { return };
        if self.kb_dirty {
            if self.kb_last_edit.elapsed() >= Duration::from_millis(500) {
                let _ = self.tx.send(Msg::Keyboard { id, change: KeyboardChange::Lighting { lighting: l } });
                self.kb_dirty = false;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        } else {
            let stored = st.and_then(|s| s.keyboards.iter().find(|k| k.id == id)).and_then(|k| k.lighting);
            if stored == Some(l) || self.kb_last_edit.elapsed() >= Duration::from_secs(5) {
                self.kb_edit = None;
            }
        }
    }
}

fn fan_mode_ui(ui: &mut egui::Ui, id: &str, mode: &mut FanMode, floor: u8, temps: (Option<f32>, Option<f32>, Option<f32>)) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let is_curve = matches!(mode, FanMode::Curve { .. });
        if ui.selectable_label(!is_curve, "Fixed").clicked() && is_curve {
            *mode = FanMode::Fixed { percent: floor.max(60) };
            changed = true;
        }
        if ui.selectable_label(is_curve, "Curve").clicked() && !is_curve {
            *mode = if id == "gpu" {
                default_gpu_curve()
            } else {
                FanMode::Curve { source: TempSource::Liquid, points: vec![(30.0, floor), (40.0, 100)] }
            };
            changed = true;
        }
        ui.label(RichText::new(format!("minimum {floor}%")).weak());
    });
    match mode {
        FanMode::Fixed { percent } => {
            changed |= ui.add(egui::Slider::new(percent, floor..=100).suffix(" %")).changed();
        }
        FanMode::Curve { source, points } => {
            ui.horizontal(|ui| {
                ui.label("Temperature");
                egui::ComboBox::from_id_salt(format!("{id}-src")).selected_text(source_name(*source)).show_ui(ui, |ui| {
                    for s in [TempSource::Liquid, TempSource::Cpu, TempSource::Gpu, TempSource::Max] {
                        changed |= ui.selectable_value(source, s, source_name(s)).changed();
                    }
                });
                if ui.button("+ point").clicked() && points.len() < 8 {
                    let t = points.iter().map(|p| p.0).fold(f32::MIN, f32::max) + 3.0;
                    points.push((t, 100));
                    changed = true;
                }
                if ui.button("− point").clicked() && points.len() > 2 {
                    points.pop();
                    changed = true;
                }
            });
            let current = match source {
                TempSource::Liquid => temps.0,
                TempSource::Cpu => temps.1,
                TempSource::Gpu => temps.2,
                TempSource::Max => None,
            };
            let range = if *source == TempSource::Liquid { (20.0, 60.0) } else { (20.0, 100.0) };
            changed |= fan_curve::editor(ui, id, points, floor, id == "gpu", range, current);
        }
    }
    changed
}

fn gpu_fans_ui(ui: &mut egui::Ui, gpu: &mut Option<FanMode>, temps: (Option<f32>, Option<f32>, Option<f32>)) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.selectable_label(gpu.is_none(), "Auto (card default)").clicked() && gpu.is_some() {
            *gpu = None;
            changed = true;
        }
        if ui.selectable_label(gpu.is_some(), "Custom").clicked() && gpu.is_none() {
            *gpu = Some(default_gpu_curve());
            changed = true;
        }
    });
    match gpu {
        None => {
            ui.label(RichText::new("The card controls its fans, including stopping them when cool.").weak());
        }
        Some(mode) => {
            ui.label(
                RichText::new(format!(
                    "Drag a point to the bottom (0 %) to let the fans stop there; anything else is at least                      {MIN_GPU_FAN_PERCENT}% (card minimum). Full speed at 85 °C or if the sensor fails."
                ))
                .weak(),
            );
            changed |= fan_mode_ui(ui, "gpu", mode, MIN_GPU_FAN_PERCENT, temps);
        }
    }
    changed
}

const PORT_TYPES: [(u16, &str); 8] = [
    (0, "Nothing"),
    (4, "ML PRO fan (4 LEDs)"),
    (8, "8-LED fan (AIO / AF / ML / SP Elite)"),
    (10, "LED strip (10)"),
    (12, "HD fan (12)"),
    (16, "LL fan (16)"),
    (29, "AIO pump (29)"),
    (34, "QL fan (34)"),
];

/// Daemon device id of the Commander Core.
const CC_ID: &str = "commander_core";

/// "Port 3 · LL fan (16)" style name of a Commander Core LED port.
fn port_label(p: usize, leds: u16) -> String {
    let base = if p == 0 { "Pump port".to_string() } else { format!("Port {p}") };
    let kind = PORT_TYPES.iter().find(|t| t.0 == leds).map_or(format!("{leds} LEDs"), |t| t.1.to_string());
    format!("{base} · {kind}")
}

fn led_ports_ui(ui: &mut egui::Ui, ports: &mut Vec<Option<u16>>, detected: &[u16], driven: &[u16]) -> bool {
    let mut changed = false;
    if ports.len() < driven.len() {
        ports.resize(driven.len(), None);
    }
    egui::Grid::new("cc-ports").spacing([16.0, 6.0]).show(ui, |ui| {
        for (i, slot) in ports.iter_mut().enumerate().take(driven.len()) {
            ui.label(if i == 0 { "Pump port".to_string() } else { format!("Port {i}") });
            let name = |n: u16| PORT_TYPES.iter().find(|t| t.0 == n).map_or(format!("{n} LEDs"), |t| t.1.to_string());
            let auto = format!("Auto (hub says {}, using {})", detected.get(i).copied().unwrap_or(0), name(driven[i]));
            let text = slot.map_or(auto.clone(), name);
            egui::ComboBox::from_id_salt(("cc-port", i)).selected_text(text).width(320.0).show_ui(ui, |ui| {
                changed |= ui.selectable_value(slot, None, auto).changed();
                for (n, label) in PORT_TYPES {
                    changed |= ui.selectable_value(slot, Some(n), label).changed();
                }
            });
            ui.end_row();
        }
    });
    changed
}

fn source_name(s: TempSource) -> &'static str {
    match s {
        TempSource::Liquid => "Liquid (AIO)",
        TempSource::Cpu => "CPU",
        TempSource::Gpu => "GPU",
        TempSource::Max => "Hottest of all",
    }
}

impl eframe::App for App {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (status, error, remote_cfg) = {
            let l = self.link.lock().unwrap();
            (l.status.clone(), l.error.clone(), l.config.clone())
        };
        // Take the daemon's copy unless the user is editing (pending send or edited in the last 2 s).
        if self.cfg.is_none() || (!self.dirty && self.last_edit.elapsed() > Duration::from_secs(2) && remote_cfg.is_some()) {
            self.cfg = remote_cfg;
        }
        if status.is_none() {
            self.cfg = None;
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Cool'n RGB").strong());
                if let Some(cfg) = self.cfg.as_mut() {
                    ui.add_space(16.0);
                    let label = if cfg.lights_on { "Lights: on" } else { "Lights: off" };
                    if ui.selectable_label(cfg.lights_on, label).clicked() {
                        cfg.lights_on = !cfg.lights_on;
                        self.dirty = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &status {
                    Some(s) => {
                        ui.label(RichText::new(format!("service running · {:.1} MB", s.working_set_kb as f32 / 1024.0)).weak());
                    }
                    None => {
                        ui.label(RichText::new("service not running").color(Color32::from_rgb(230, 120, 80)));
                    }
                });
            });
        });

        egui::Panel::left("nav").exact_size(130.0).show(ui, |ui| {
            ui.add_space(8.0);
            for (t, label) in [(Tab::Lighting, "Lighting"), (Tab::Fans, "Fans"), (Tab::Devices, "Devices"), (Tab::Settings, "Settings")] {
                if ui.selectable_label(self.tab == t, RichText::new(label).size(16.0)).clicked() {
                    self.tab = t;
                }
                ui.add_space(4.0);
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let (Some(st), true) = (status.clone(), self.cfg.is_some()) else {
                ui.add_space(30.0);
                ui.label("The background service (coolrgbd.exe) is not reachable.");
                if let Some(e) = &error {
                    ui.label(RichText::new(e).weak());
                }
                ui.add_space(8.0);
                if ui.button("Start background service").clicked() {
                    start_daemon();
                    self.started_daemon = true;
                }
                if self.started_daemon {
                    ui.label(RichText::new("Starting… approve the administrator prompt if Windows shows one.").weak());
                }
                return;
            };
            match self.tab {
                Tab::Lighting => self.lighting(ui, &st),
                Tab::Fans => self.fans(ui, &st),
                Tab::Devices => self.devices(ui, &st),
                Tab::Settings => {
                    let cfg = self.cfg.as_mut().unwrap();
                    if ui.checkbox(&mut cfg.autostart, "Start with Windows").changed() {
                        self.dirty = true;
                    }
                    ui.label(RichText::new("Uses a Task Scheduler task so the service starts elevated without a prompt.").weak());
                    ui.add_space(12.0);
                    ui.label(format!(
                        "Safety floors: fans never below {MIN_FAN_PERCENT}%, pump never below {MIN_PUMP_PERCENT}%. \
                         Full speed above 50 °C liquid or 95 °C CPU, or when a curve's sensor is missing."
                    ));
                    ui.add_space(12.0);
                    ui.label(format!("Background service memory: {:.1} MB", st.working_set_kb as f32 / 1024.0));
                    ui.label(RichText::new("Closing this window frees the control panel's memory; lighting and fans keep running.").weak());
                    ui.add_space(12.0);
                    if ui.button("Stop background service").on_hover_text("Devices go back to their stored hardware profiles").clicked() {
                        let _ = self.tx.send(Msg::Quit);
                    }
                }
            }
        });

        self.flush_keyboard_edit(ui.ctx(), status.as_ref());
        if self.dirty {
            self.last_edit = Instant::now();
        }
        // Send edits at most every 120 ms (slider drags fire every frame).
        if self.dirty && self.last_send.elapsed() >= Duration::from_millis(120) {
            if let Some(c) = &self.cfg {
                let _ = self.tx.send(Msg::SetConfig(c.clone()));
                self.dirty = false;
                self.last_send = Instant::now();
            }
        } else if self.dirty {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }

        if ui.ctx().input(|i| i.viewport().close_requested()) {
            let pos = ui.ctx().input(|i| i.viewport().outer_rect).map(|r| (r.min.x, r.min.y));
            if let (Some(mut c), Some(p)) = (self.cfg.clone(), pos) {
                c.gui_pos = Some(p);
                let _ = call(&Request::SetConfig { config: c });
            }
        }
    }
}

fn arg_value(name: &str) -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != name);
    args.next()?;
    args.next()
}

/// `--pos x,y` opens the window at that screen position.
fn arg_pos() -> Option<(f32, f32)> {
    let v = arg_value("--pos")?;
    let (x, y) = v.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Bring an already open control panel to the front instead of opening a second one.
fn focus_existing() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE};
    let title: Vec<u16> = "Cool'n RGB\0".encode_utf16().collect();
    let h = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe {
        ShowWindow(h, SW_RESTORE);
        SetForegroundWindow(h);
    }
    true
}

/// A saved position from a monitor that is no longer connected would open the window off-screen.
fn on_screen(p: (f32, f32)) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let m = |i| unsafe { GetSystemMetrics(i) } as f32;
    let (x, y) = (m(SM_XVIRTUALSCREEN), m(SM_YVIRTUALSCREEN));
    p.0 >= x - 50.0 && p.1 >= y - 10.0 && p.0 < x + m(SM_CXVIRTUALSCREEN) - 200.0 && p.1 < y + m(SM_CYVIRTUALSCREEN) - 100.0
}

fn main() -> eframe::Result {
    if focus_existing() {
        return Ok(());
    }
    let saved = match call(&Request::GetConfig) {
        Ok(Response::Config { config }) => config.gui_pos.filter(|&p| on_screen(p)),
        _ => None,
    };
    let mut vp = egui::ViewportBuilder::default()
        .with_title("Cool'n RGB")
        .with_inner_size([820.0, 600.0])
        .with_min_inner_size([640.0, 460.0])
        .with_icon(egui::IconData { rgba: coolrgb_core::effects::icon_rgba(64), width: 64, height: 64 });
    if let Some(p) = arg_pos().or(saved) {
        vp = vp.with_position(p);
    }
    let opts = eframe::NativeOptions { viewport: vp, ..Default::default() };
    eframe::run_native("Cool'n RGB", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
