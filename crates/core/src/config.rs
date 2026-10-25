use crate::effects::Effect;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Hard floors agreed for this build. The daemon never sends a lower duty.
pub const MIN_FAN_PERCENT: u8 = 40;
pub const MIN_PUMP_PERCENT: u8 = 70;

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
    pub pump: FanMode,
    pub fans: FanMode,
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
            pump: FanMode::Curve {
                source: TempSource::Liquid,
                points: vec![(30.0, 70), (36.0, 80), (40.0, 100)],
            },
            fans: FanMode::Curve {
                source: TempSource::Liquid,
                points: vec![(28.0, 40), (32.0, 50), (36.0, 70), (40.0, 90), (43.0, 100)],
            },
            autostart: false,
            gui_pos: None,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("coolrgb")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        std::fs::read(config_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
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
    fn max_source_skips_missing_sensors() {
        let m = FanMode::Curve { source: TempSource::Max, points: vec![(30.0, 40), (40.0, 100)] };
        assert_eq!(m.duty(Some(30.0), None, Some(40.0)), Some(100));
        assert_eq!(m.duty(None, None, None), None);
    }
}
