//! Corsair: iCUE Commander Core hub, "V2"/Bragi peripherals, Vengeance RGB Pro DDR4.
//!
//! The mouse settings below are what the daemon and the control panel exchange over IPC. They are
//! plain data and always compiled, so the GUI builds without the USB drivers behind the `corsair` feature.
use crate::effects::Rgb;
use serde::{Deserialize, Serialize};

#[cfg(feature = "corsair")]
pub mod bragi;
#[cfg(feature = "corsair")]
pub mod commander_core;
#[cfg(feature = "corsair")]
pub mod dram;

/// Hardware DPI stages stored on the mouse; the DPI button steps through them.
pub const DPI_STAGES: usize = 5;
/// The mouse rejects DPI values outside this range or off the step.
pub const DPI_MIN: u16 = 200;
pub const DPI_MAX: u16 = 10000;
pub const DPI_STEP: u16 = 200;
pub const POLLING_HZ: [u16; 4] = [125, 250, 500, 1000];
pub const SLEEP_MINUTES: [u16; 7] = [1, 2, 5, 10, 15, 30, 60];

/// Nearest DPI value the mouse accepts.
pub fn valid_dpi(dpi: u16) -> u16 {
    let d = dpi.clamp(DPI_MIN, DPI_MAX);
    (d + DPI_STEP / 2) / DPI_STEP * DPI_STEP
}

/// What the control panel (or the mouse's DPI button) can change on a mouse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MouseChange {
    /// DPI of every stage, `DPI_STAGES` values.
    DpiStages { dpi: Vec<u16> },
    /// Indicator color of every stage, `DPI_STAGES` values.
    DpiColors { colors: Vec<Rgb> },
    /// Make a stage active, 0-based.
    Stage { stage: u8 },
    /// Next stage, wrapping around (the DPI button).
    NextStage,
    PollingRate { hz: u16 },
    AngleSnapping { on: bool },
    Sleep { minutes: u16 },
}

/// Mouse state reported to the control panel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MouseState {
    pub id: String,
    pub dpi_stages: Vec<u16>,
    /// Indicator color of each stage, shown on the DPI LED.
    pub dpi_colors: Vec<Rgb>,
    /// Active stage, 0-based.
    pub stage: u8,
    pub polling_hz: Option<u16>,
    pub angle_snapping: Option<bool>,
    pub sleep_minutes: Option<u16>,
    /// Percent.
    pub battery: Option<u8>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_snaps_to_accepted_values() {
        assert_eq!(valid_dpi(1100), 1200);
        assert_eq!(valid_dpi(1099), 1000);
        assert_eq!(valid_dpi(50), DPI_MIN);
        assert_eq!(valid_dpi(20000), DPI_MAX);
    }
}
