use crate::effects::Effect;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Hard floors agreed for this build. The daemon never sends a lower duty.
pub const MIN_FAN_PERCENT: u8 = 40;
pub const MIN_PUMP_PERCENT: u8 = 70;
/// Lowest duty NVIDIA allows on this card's fans.
pub const MIN_GPU_FAN_PERCENT: u8 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempSource {
    Liquid,
    Cpu,
    Gpu,
    /// Highest of the three after each is mapped through the curve.
    Max,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FanMode {
    Fixed { percent: u8 },
    Curve { source: TempSource, points: Vec<(f32, u8)> },
}

impl FanMode {
    /// Duty for the given temperatures; `None` when the source sensor is missing.
    pub fn duty(&self, liquid: Option<f32>, cpu: Option<f32>, gpu: Option<f32>) -> Option<u8> {
        match self {
            FanMode::Fixed { percent } => Some(*percent),
            FanMode::Curve { source, points } => {
                let eval = |t: Option<f32>| t.map(|t| interpolate(points, t));
                match source {
                    TempSource::Liquid => eval(liquid),
                    TempSource::Cpu => eval(cpu),
                    TempSource::Gpu => eval(gpu),
                    TempSource::Max => [eval(liquid), eval(cpu), eval(gpu)].into_iter().flatten().max(),
                }
            }
        }
    }
}

/// Linear interpolation over points sorted by temperature; clamps at both ends.
pub fn interpolate(points: &[(f32, u8)], t: f32) -> u8 {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    match pts.as_slice() {
        [] => 100,
        [only] => only.1,
        _ => {
            if t <= pts[0].0 {
                return pts[0].1;
            }
            for w in pts.windows(2) {
                let ((t0, d0), (t1, d1)) = (w[0], w[1]);
                if t <= t1 {
                    let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                    return (d0 as f32 + (d1 as f32 - d0 as f32) * f).round() as u8;
                }
            }
            pts[pts.len() - 1].1
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Master switch: false renders every device black without touching the effects.
    pub lights_on: bool,
    /// 0..=100
    pub brightness: u8,
    pub effect: Effect,
    /// Device id -> effect override.
    pub per_device: BTreeMap<String, Effect>,
    /// Devices left to run the effect stored on them (e.g. a keyboard's built-in effect) while lights are on.
    pub onboard_lighting: BTreeSet<String>,
    pub pump: FanMode,
    pub fans: FanMode,
    /// `None` = the graphics card's own automatic curve (including zero-RPM).
    pub gpu_fans: Option<FanMode>,
    /// Commander Core LED count per port (0 = pump); `None`/missing = use the hub's detection.
    pub led_ports: Vec<Option<u16>>,
    pub autostart: bool,
    /// Last GUI window position (screen coordinates).
    pub gui_pos: Option<(f32, f32)>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            lights_on: true,
            brightness: 100,
            effect: Effect::default(),
            per_device: BTreeMap::new(),
            onboard_lighting: BTreeSet::new(),
            pump: FanMode::Curve {
                source: TempSource::Liquid,
                points: vec![(30.0, 70), (36.0, 80), (40.0, 100)],
            },
            fans: FanMode::Curve {
                source: TempSource::Liquid,
                points: vec![(28.0, 40), (32.0, 50), (36.0, 70), (40.0, 90), (43.0, 100)],
            },
            gpu_fans: None,
            led_ports: Vec::new(),
            autostart: false,
            gui_pos: None,
        }
    }
}

/// Machine-wide `%ProgramData%\CoolRGB`. Per-user `%APPDATA%` is unreliable here: a service launched
/// from an MSIX-packaged app (e.g. a terminal inside one) gets a redirected private AppData, so it and
/// the logon task would each read a different config.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("CoolRGB")
}

/// Where versions before 0.1.1 kept the config.
fn legacy_config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("coolrgb").join("config.json"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Starting point offered when GPU fans are switched to a curve.
pub fn default_gpu_curve() -> FanMode {
    FanMode::Curve {
        source: TempSource::Gpu,
        points: vec![(49.0, 0), (50.0, 30), (60.0, 40), (70.0, 60), (78.0, 80), (84.0, 100)],
    }
}

pub struct Preset {
    pub name: &'static str,
    pub pump: FanMode,
    pub fans: FanMode,
    pub gpu_fans: Option<FanMode>,
}

fn curve(source: TempSource, points: &[(f32, u8)]) -> FanMode {
    FanMode::Curve { source, points: points.to_vec() }
}

/// Ready-made cooling profiles. Case fans and pump follow liquid temperature: it moves slowly, so
/// fans don't rev up and down with every short CPU load spike.
pub fn presets() -> Vec<Preset> {
    use TempSource::{Gpu, Liquid};
    vec![
        Preset {
            name: "Quiet",
            pump: curve(Liquid, &[(32.0, 70), (38.0, 75), (42.0, 90), (45.0, 100)]),
            fans: curve(Liquid, &[(31.0, 40), (35.0, 45), (38.0, 55), (41.0, 70), (44.0, 85), (47.0, 100)]),
            // Fans off below 55 °C (card zero-RPM), gentle ramp, full speed only near the throttle point.
            gpu_fans: Some(curve(Gpu, &[(55.0, 0), (56.0, 30), (65.0, 35), (72.0, 50), (78.0, 70), (83.0, 100)])),
        },
        Preset {
            name: "Balanced",
            pump: curve(Liquid, &[(30.0, 70), (36.0, 80), (40.0, 100)]),
            fans: curve(Liquid, &[(28.0, 40), (32.0, 50), (36.0, 70), (40.0, 90), (43.0, 100)]),
            gpu_fans: None,
        },
        Preset {
            name: "Performance",
            pump: curve(Liquid, &[(28.0, 85), (33.0, 100)]),
            fans: curve(Liquid, &[(28.0, 50), (32.0, 65), (36.0, 85), (39.0, 100)]),
            gpu_fans: Some(curve(Gpu, &[(40.0, 30), (60.0, 50), (70.0, 75), (78.0, 100)])),
        },
    ]
}

impl Config {
    pub fn load() -> Config {
        let parse = |p: PathBuf| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Config>(&b).ok());
        if let Some(c) = parse(config_path()) {
            return c;
        }
        match legacy_config_path().and_then(parse) {
            Some(c) => {
                let _ = c.save();
                c
            }
            None => Config::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let tmp = config_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(tmp, config_path())
    }

    pub fn effect_for(&self, device_id: &str) -> &Effect {
        self.per_device.get(device_id).unwrap_or(&self.effect)
    }

    /// Effect of one port of a multi-port device: the port's own, else the device's, else the global one.
    pub fn effect_for_port(&self, device_id: &str, port: usize) -> &Effect {
        self.per_device.get(&port_key(device_id, port)).unwrap_or_else(|| self.effect_for(device_id))
    }
}

/// `per_device` key for one port of a multi-port device, e.g. `commander_core:3`.
pub fn port_key(device_id: &str, port: usize) -> String {
    format!("{device_id}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_clamps_and_blends() {
        let p = [(30.0, 40), (40.0, 100)];
        assert_eq!(interpolate(&p, 20.0), 40);
        assert_eq!(interpolate(&p, 35.0), 70);
        assert_eq!(interpolate(&p, 50.0), 100);
    }

    #[test]
    fn port_effect_falls_back_to_device_then_global() {
        let mut c = Config::default();
        assert_eq!(c.effect_for_port("hub", 2), &c.effect);
        c.per_device.insert("hub".into(), Effect::Off);
        assert_eq!(c.effect_for_port("hub", 2), &Effect::Off);
        c.per_device.insert(port_key("hub", 2), Effect::Rainbow { period_s: 3.0 });
        assert_eq!(c.effect_for_port("hub", 2), &Effect::Rainbow { period_s: 3.0 });
        assert_eq!(c.effect_for_port("hub", 1), &Effect::Off);
    }

    #[test]
    fn max_source_skips_missing_sensors() {
        let m = FanMode::Curve { source: TempSource::Max, points: vec![(30.0, 40), (40.0, 100)] };
        assert_eq!(m.duty(Some(30.0), None, Some(40.0)), Some(100));
        assert_eq!(m.duty(None, None, None), None);
    }
}
