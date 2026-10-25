//! EVision "V2" keyboards in direct mode (DesignedByGG Ironclad V3). See docs/PROTOCOLS.md.
use crate::effects::Rgb;
use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const VID: u16 = 0x320f;
/// (pid, name) of supported keyboards.
pub const DEVICES: [(u16, &str); 2] = [(0x511e, "DesignedByGG Ironclad V3"), (0x5041, "DesignedByGG Ironclad")];

const REPORT_ID: u8 = 0x04;
const PACKET: usize = 64;
/// Payload bytes per packet (after the 8-byte header).
const CHUNK: usize = PACKET - 8;
/// Color slots are laid out column by column, 6 rows each.
pub const ROWS: usize = 6;

const CMD_BEGIN_CONFIGURE: u8 = 0x01;
const CMD_END_CONFIGURE: u8 = 0x02;
const CMD_READ_CAPABILITIES: u8 = 0x03;
const CMD_READ_CONFIG: u8 = 0x05;
const CMD_WRITE_CONFIG: u8 = 0x06;
const CMD_SEND_DYNAMIC_COLORS: u8 = 0x12;
const CMD_END_DYNAMIC_COLORS: u8 = 0x13;

/// Config byte 0 = active onboard profile; profile `p` starts at `1 + p * 0x40`.
const PROFILE_SIZE: u16 = 0x40;
/// Lighting parameters at the start of a profile: mode, brightness, speed, direction, random flag,
/// RGB, then mode-specific bytes we keep as they are.
const LIGHTING_LEN: usize = 18;
pub const PROFILES: u8 = 3;
pub const MAX_BRIGHTNESS: u8 = 4;
/// Speed 0 is the fastest.
pub const SLOWEST: u8 = 5;
/// Profile byte holding the USB polling rate, as an index into `POLLING_HZ`.
const POLLING_OFFSET: u16 = 0x15;
/// Profile byte holding the key debounce time, as an index into `DEBOUNCE_MS`.
const DEBOUNCE_OFFSET: u16 = 0x18;
pub const POLLING_HZ: [u16; 4] = [1000, 500, 250, 125];
pub const DEBOUNCE_MS: [u8; 4] = [5, 10, 15, 20];
/// The full frame is re-sent this often, so direct mode recovers if another program (the stock app
/// does on every save) switches the keyboard back to its stored effect.
const FULL_REFRESH: Duration = Duration::from_secs(1);

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

/// Direct colors fall back to the stored effect unless refreshed about every 200 ms.
pub const KEEPALIVE: Duration = Duration::from_millis(150);

pub struct EvisionV2 {
    dev: HidDevice,
    pub name: &'static str,
    /// Color slots in the keyboard's buffer (some are gaps between keys).
    pub slots: usize,
    /// Built-in effect codes from the capabilities block.
    pub modes: Vec<u8>,
    /// True while we stream colors; false while the keyboard runs its own effect.
    pub direct: bool,
    /// Last colors sent, one per column.
    frame: Vec<Rgb>,
    last_write: Instant,
    last_full: Instant,
    /// Hand the keyboard back to its stored effect when dropped.
    pub restore_on_drop: bool,
}

impl EvisionV2 {
    pub fn open_all(api: &HidApi) -> Vec<Result<EvisionV2, String>> {
        let mut out = Vec::new();
        for &(pid, name) in &DEVICES {
            for info in api.device_list().filter(|d| {
                d.vendor_id() == VID && d.product_id() == pid && d.usage_page() == 0xff1c && d.usage() == 0x92
            }) {
                out.push(info.open_device(api).map_err(|e| format!("{name}: {e}")).and_then(|dev| EvisionV2::init(dev, name)));
            }
        }
        out
    }

    fn init(dev: HidDevice, name: &'static str) -> Result<EvisionV2, String> {
        let mut d = EvisionV2 {
            dev,
            name,
            slots: 0,
            modes: Vec::new(),
            direct: false,
            frame: Vec::new(),
            last_write: Instant::now(),
            last_full: Instant::now(),
            restore_on_drop: true,
        };
        d.drain();
        let caps = d.read_caps()?;
        if caps.len() < 7 || caps[0] != 0xaa || caps[1] != 0x55 {
            return Err(format!("{name}: unexpected capabilities {caps:02x?}"));
        }
        d.slots = caps[5] as usize;
        // Supported effect codes follow at byte 31, zero-terminated (seen: 01..0f 11 10 12 14).
        d.modes = caps.iter().skip(31).take_while(|&&m| m != 0).copied().collect();
        if d.slots == 0 {
            return Err(format!("{name}: no LED slots reported"));
        }
        Ok(d)
    }

    fn drain(&self) {
        let mut r = [0u8; PACKET];
        while matches!(self.dev.read_timeout(&mut r, 3), Ok(n) if n > 0) {}
    }

    /// One request/response. `size` is the payload length to read or write; returns the reply payload.
    fn query(&self, cmd: u8, offset: u16, data: &[u8], size: usize) -> Result<Vec<u8>, String> {
        let mut w = [0u8; PACKET];
        w[0] = REPORT_ID;
        w[3] = cmd;
        w[4] = size as u8;
        w[5..7].copy_from_slice(&offset.to_le_bytes());
        w[8..8 + data.len()].copy_from_slice(data);
        let sum = w[3..].iter().fold(0u16, |s, &b| s.wrapping_add(b as u16));
        w[1..3].copy_from_slice(&sum.to_le_bytes());
        self.dev.write(&w).map_err(|e| format!("{}: write {e}", self.name))?;
        // Every open handle sees every reply (ICL03.exe, a probe), so skip replies to other requests.
        let mut r = [0u8; PACKET];
        let mut matched = false;
        for _ in 0..8 {
            let n = self.dev.read_timeout(&mut r, 200).map_err(|e| format!("{}: read {e}", self.name))?;
            if n == 0 {
                return Err(format!("{}: timeout", self.name));
            }
            if n == PACKET && r[0] == REPORT_ID && r[3..7] == w[3..7] {
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(format!("{}: no reply to {cmd:02x}", self.name));
        }
        if r[7] != 0 {
            return Err(format!("{}: command {cmd:02x} status {:02x}", self.name, r[7]));
        }
        let n = (r[4] as usize).min(CHUNK);
        Ok(r[8..8 + n].to_vec())
    }

    /// Raw capabilities block (first 56 bytes).
    pub fn read_caps(&self) -> Result<Vec<u8>, String> {
        self.query(CMD_READ_CAPABILITIES, 0, &[], CHUNK)
    }

    /// Read `len` bytes of the keyboard's stored configuration starting at `offset`.
    pub fn read_config(&self, offset: u16, len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(CHUNK);
            let part = self.query(CMD_READ_CONFIG, offset + out.len() as u16, &[], n)?;
            if part.is_empty() {
                break;
            }
            out.extend_from_slice(&part);
        }
        Ok(out)
    }

    /// Write stored configuration; the keyboard keeps it across power cycles.
    fn write_config(&self, offset: u16, data: &[u8]) -> Result<(), String> {
        self.query(CMD_BEGIN_CONFIGURE, 0, &[], 0)?;
        let mut result = Ok(());
        for (i, part) in data.chunks(CHUNK).enumerate() {
            result = self.query(CMD_WRITE_CONFIG, offset + (i * CHUNK) as u16, part, part.len()).map(drop);
            if result.is_err() {
                break;
            }
        }
        self.query(CMD_END_CONFIGURE, 0, &[], 0)?;
        result
    }

    /// Active onboard profile, 0-based.
    pub fn profile(&self) -> Result<u8, String> {
        let p = self.read_config(0, 1)?.first().copied().unwrap_or(0);
        Ok(if p < PROFILES { p } else { 0 })
    }

    pub fn set_profile(&self, profile: u8) -> Result<(), String> {
        if profile >= PROFILES {
            return Err(format!("{}: no profile {}", self.name, profile + 1));
        }
        self.write_config(0, &[profile])
    }

    fn profile_offset(&self) -> Result<u16, String> {
        Ok(1 + self.profile()? as u16 * PROFILE_SIZE)
    }

    /// Built-in effect of the active profile.
    pub fn onboard_lighting(&self) -> Result<OnboardLighting, String> {
        let b = self.read_config(self.profile_offset()?, 8)?;
        if b.len() < 8 {
            return Err(format!("{}: short profile read", self.name));
        }
        Ok(OnboardLighting {
            mode: b[0],
            brightness: b[1].min(MAX_BRIGHTNESS),
            speed: b[2].min(SLOWEST),
            direction: b[3] & 1,
            random_colors: b[4] != 0,
            color: Rgb(b[5], b[6], b[7]),
        })
    }

    /// Store a built-in effect in the active profile. Mode-specific bytes after the color stay as they are.
    pub fn set_onboard_lighting(&self, l: &OnboardLighting) -> Result<(), String> {
        if !self.modes.contains(&l.mode) {
            return Err(format!("{}: effect {:02x} not supported", self.name, l.mode));
        }
        let offset = self.profile_offset()?;
        let mut b = self.read_config(offset, LIGHTING_LEN)?;
        if b.len() < LIGHTING_LEN {
            return Err(format!("{}: short profile read", self.name));
        }
        b[..8].copy_from_slice(&[
            l.mode,
            l.brightness.min(MAX_BRIGHTNESS),
            l.speed.min(SLOWEST),
            l.direction & 1,
            if l.random_colors { 0xff } else { 0 },
            l.color.0,
            l.color.1,
            l.color.2,
        ]);
        self.write_config(offset, &b)
    }

    /// (polling rate Hz, debounce ms) of the active profile.
    pub fn timing(&self) -> Result<(u16, u8), String> {
        let off = self.profile_offset()?;
        let b = self.read_config(off + POLLING_OFFSET, (DEBOUNCE_OFFSET - POLLING_OFFSET + 1) as usize)?;
        let last = (DEBOUNCE_OFFSET - POLLING_OFFSET) as usize;
        if b.len() <= last {
            return Err(format!("{}: short profile read", self.name));
        }
        let hz = POLLING_HZ.get(b[0] as usize).copied().unwrap_or(POLLING_HZ[0]);
        let ms = DEBOUNCE_MS.get(b[last] as usize).copied().unwrap_or(DEBOUNCE_MS[0]);
        Ok((hz, ms))
    }

    pub fn apply(&self, change: &KeyboardChange) -> Result<(), String> {
        let index = |found: Option<usize>, what: &str| found.map(|i| i as u8).ok_or_else(|| format!("{}: unsupported {what}", self.name));
        match change {
            KeyboardChange::Profile { profile } => self.set_profile(*profile),
            KeyboardChange::Lighting { lighting } => self.set_onboard_lighting(lighting),
            KeyboardChange::PollingRate { hz } => {
                let i = index(POLLING_HZ.iter().position(|v| v == hz), "polling rate")?;
                self.write_config(self.profile_offset()? + POLLING_OFFSET, &[i])
            }
            KeyboardChange::Debounce { ms } => {
                let i = index(DEBOUNCE_MS.iter().position(|v| v == ms), "debounce time")?;
                self.write_config(self.profile_offset()? + DEBOUNCE_OFFSET, &[i])
            }
        }
    }

    pub fn state(&self, id: &str) -> KeyboardState {
        let read = || -> Result<(u8, OnboardLighting, (u16, u8)), String> {
            Ok((self.profile()?, self.onboard_lighting()?, self.timing()?))
        };
        match read() {
            Ok((profile, lighting, (hz, ms))) => KeyboardState {
                id: id.into(),
                profile,
                lighting: Some(lighting),
                polling_hz: Some(hz),
                debounce_ms: Some(ms),
                modes: self.modes.clone(),
                error: None,
            },
            Err(e) => KeyboardState { id: id.into(), modes: self.modes.clone(), error: Some(e), ..Default::default() },
        }
    }

    /// Number of key columns (left to right).
    pub fn columns(&self) -> usize {
        self.slots.div_ceil(ROWS)
    }

    /// One color per column, left to right; every key in a column gets that color.
    pub fn set_columns(&mut self, cols: &[Rgb]) -> Result<(), String> {
        let mut data = vec![0u8; self.slots * 3];
        for (s, px) in data.chunks_exact_mut(3).enumerate() {
            let c = cols.get(s / ROWS).copied().unwrap_or(Rgb::BLACK);
            px.copy_from_slice(&[c.0, c.1, c.2]);
        }
        for (i, part) in data.chunks(CHUNK).enumerate() {
            self.query(CMD_SEND_DYNAMIC_COLORS, (i * CHUNK) as u16, part, part.len())?;
        }
        self.frame = cols.to_vec();
        self.last_write = Instant::now();
        self.last_full = self.last_write;
        self.direct = true;
        Ok(())
    }

    /// Re-send the first slot unchanged so the keyboard stays in direct mode; the whole frame now and then.
    pub fn refresh(&mut self) -> Result<(), String> {
        if self.last_full.elapsed() >= FULL_REFRESH {
            let frame = std::mem::take(&mut self.frame);
            return self.set_columns(&frame);
        }
        let c = self.frame.first().copied().unwrap_or(Rgb::BLACK);
        self.query(CMD_SEND_DYNAMIC_COLORS, 0, &[c.0, c.1, c.2], 3)?;
        self.last_write = Instant::now();
        Ok(())
    }

    pub fn since_last_write(&self) -> Duration {
        self.last_write.elapsed()
    }

    /// Leave direct mode: the keyboard shows its stored effect again.
    pub fn hardware_mode(&mut self) -> Result<(), String> {
        self.query(CMD_END_DYNAMIC_COLORS, 0, &[], 0)?;
        self.direct = false;
        Ok(())
    }
}

impl Drop for EvisionV2 {
    fn drop(&mut self) {
        if self.restore_on_drop && self.direct {
            let _ = self.hardware_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checksum_covers_command_bytes() {
        let mut w = [0u8; super::PACKET];
        w[3] = 0x03;
        w[4] = 7;
        let sum = w[3..].iter().fold(0u16, |s, &b| s.wrapping_add(b as u16));
        assert_eq!(sum.to_le_bytes(), [0x0a, 0x00]);
    }
}
