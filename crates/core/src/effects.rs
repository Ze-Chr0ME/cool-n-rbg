use serde::{Deserialize, Serialize};

/// 8-bit RGB color, serialized as `[r, g, b]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);

    pub fn scale(self, f: f32) -> Rgb {
        let f = f.clamp(0.0, 1.0);
        Rgb(
            (self.0 as f32 * f).round() as u8,
            (self.1 as f32 * f).round() as u8,
            (self.2 as f32 * f).round() as u8,
        )
    }

    /// `h` in turns (0..1), full saturation and value.
    pub fn from_hue(h: f32) -> Rgb {
        let h = h.rem_euclid(1.0) * 6.0;
        let x = 1.0 - (h % 2.0 - 1.0).abs();
        let (r, g, b) = match h as u32 {
            0 => (1.0, x, 0.0),
            1 => (x, 1.0, 0.0),
            2 => (0.0, 1.0, x),
            3 => (0.0, x, 1.0),
            4 => (x, 0.0, 1.0),
            _ => (1.0, 0.0, x),
        };
        Rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    Off,
    Static { color: Rgb },
    Breathing { color: Rgb, period_s: f32 },
    /// Whole device cycles through the hue wheel.
    Rainbow { period_s: f32 },
    /// Hue wheel spread across the LEDs of each device, rotating.
    Wave { period_s: f32 },
}

impl Default for Effect {
    fn default() -> Self {
        Effect::Static { color: Rgb(0, 160, 255) }
    }
}

impl Effect {
    pub fn is_animated(&self) -> bool {
        matches!(self, Effect::Breathing { .. } | Effect::Rainbow { .. } | Effect::Wave { .. })
    }

    /// Fill `out` with this effect at time `t` seconds, scaled by `brightness` (0..1).
    pub fn render(&self, t: f32, brightness: f32, out: &mut [Rgb]) {
        let n = out.len().max(1) as f32;
        for (i, px) in out.iter_mut().enumerate() {
            *px = match *self {
                Effect::Off => Rgb::BLACK,
                Effect::Static { color } => color,
                Effect::Breathing { color, period_s } => {
                    let phase = (t / period_s.max(0.2)).fract();
                    // Smooth 0..1..0 sine pulse that never fully turns off.
                    let s = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                    color.scale(0.05 + 0.95 * s)
                }
                Effect::Rainbow { period_s } => Rgb::from_hue(t / period_s.max(0.2)),
                Effect::Wave { period_s } => Rgb::from_hue(t / period_s.max(0.2) + i as f32 / n),
            }
            .scale(brightness);
        }
    }
}

/// RGBA pixels of the app icon (rainbow ring), `n` x `n`, edges anti-aliased.
pub fn icon_rgba(n: u32) -> Vec<u8> {
    let c = (n as f32 - 1.0) / 2.0;
    let (ro, ri) = (n as f32 * 0.485, n as f32 * 0.26);
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let mut cover = 0.0;
            for s in 0..16 {
                let dx = x as f32 + ((s % 4) as f32 + 0.5) / 4.0 - 0.5 - c;
                let dy = y as f32 + ((s / 4) as f32 + 0.5) / 4.0 - 0.5 - c;
                if (ri..=ro).contains(&(dx * dx + dy * dy).sqrt()) {
                    cover += 1.0 / 16.0;
                }
            }
            let col = Rgb::from_hue((y as f32 - c).atan2(x as f32 - c) / std::f32::consts::TAU);
            out.extend_from_slice(&[col.0, col.1, col.2, (cover * 255.0) as u8]);
        }
    }
    out
}
