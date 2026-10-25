//! coolrgb: control panel. Talks to the background daemon (coolrgbd) over a named pipe; closing this
//! window frees all of its memory while lighting and fan control keep running in the daemon.
#![windows_subsystem = "windows"]

mod fan_curve;

use coolrgb_core::config::{default_gpu_curve, Config, FanMode, TempSource, MIN_FAN_PERCENT, MIN_GPU_FAN_PERCENT, MIN_PUMP_PERCENT};
use coolrgb_core::effects::{Effect, Rgb};
use coolrgb_core::ipc::{call, Request, Response, Status};
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
    started_daemon: bool,
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
            started_daemon: false,
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
                    let label = if cfg.per_device.contains_key(&d.id) { format!("{} (custom)", d.name) } else { d.name.clone() };
                    ui.selectable_value(&mut self.target, Some(d.id.clone()), label);
                }
            });
        });
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
                let mut custom = cfg.per_device.contains_key(&id);
                if ui.checkbox(&mut custom, "Use a custom effect for this device").changed() {
                    if custom {
                        cfg.per_device.insert(id.clone(), cfg.effect.clone());
                    } else {
                        cfg.per_device.remove(&id);
                    }
                    changed = true;
                }
                if let Some(e) = cfg.per_device.get_mut(&id) {
                    changed |= effect_ui(ui, e);
                    ui.add_space(8.0);
                    preview(ui, e, bright);
                } else {
                    ui.label(RichText::new("Follows the effect set for all devices.").weak());
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
                Tab::Devices => {
                    egui::Grid::new("devs").striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                        for h in ["Device", "LEDs", "State", "Details"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for d in &st.devices {
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
                    ui.add_space(10.0);
                    if ui.button("Rescan devices").clicked() {
                        let _ = self.tx.send(Msg::Rescan);
                    }
                }
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
