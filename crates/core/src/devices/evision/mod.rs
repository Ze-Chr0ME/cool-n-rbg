//! EVision keyboard controllers (sold under other brands, e.g. DesignedByGG).
//!
//! The settings below are what the daemon and the control panel exchange over IPC. They are plain
//! data and always compiled, so the GUI builds without the USB driver behind the `evision` feature.
use crate::effects::Rgb;
use serde::{Deserialize, Serialize};

#[cfg(feature = "evision")]
pub mod v2;

pub const PROFILES: u8 = 3;
pub const MAX_BRIGHTNESS: u8 = 4;
/// Speed 0 is the fastest.
pub const SLOWEST: u8 = 5;
pub const POLLING_HZ: [u16; 4] = [1000, 500, 250, 125];
pub const DEBOUNCE_MS: [u8; 4] = [5, 10, 15, 20];

/// Built-in effects (code, name). The stock app's names are in brackets where they differ a lot.
pub const MODES: [(u8, &str); 19] = [
    (0x06, "Static"),
    (0x05, "Breathing"),
    (0x04, "Spectrum cycle"),
    (0x01, "Wave (Go with the stream)"),
    (0x02, "Long wave (Clouds fly)"),
    (0x03, "Vortex (Winding paths)"),
    (0x0c, "Vertical rainbow (Swift action)"),
    (0x12, "Rainbow circle (Speed)"),
    (0x0d, "Hurricane"),
    (0x10, "Scan (Both ways)"),
    (0x11, "Surmount"),
    (0x07, "Reactive (Pass without trace)"),
    (0x08, "Ripples"),
    (0x09, "Reactive line (Stream)"),
    (0x0b, "Blooming (Flowers)"),
    (0x0a, "Stars"),
    (0x0f, "Digital times"),
    (0x0e, "Accumulate (Cartoon)"),
    (0x14, "Custom (per-key colors saved by the stock app)"),
];

/// Lighting effect stored in an onboard profile; the keyboard runs it by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardLighting {
    pub mode: u8,
    /// 0..=MAX_BRIGHTNESS (0 = off).
    pub brightness: u8,
    /// 0 (fastest)..=SLOWEST.
    pub speed: u8,
    /// 0 = counter-clockwise / leftward, 1 = clockwise / rightward.
    pub direction: u8,
    pub random_colors: bool,
    pub color: Rgb,
}

/// What the control panel can change on the keyboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyboardChange {
    Profile { profile: u8 },
    Lighting { lighting: OnboardLighting },
    PollingRate { hz: u16 },
    Debounce { ms: u8 },
}

/// Keyboard state reported to the control panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyboardState {
    pub id: String,
    /// Active onboard profile, 0-based.
    pub profile: u8,
    pub lighting: Option<OnboardLighting>,
    /// Of the active profile.
    pub polling_hz: Option<u16>,
    pub debounce_ms: Option<u8>,
    /// Built-in effect codes the keyboard lists as supported.
    pub modes: Vec<u8>,
    pub error: Option<String>,
}
