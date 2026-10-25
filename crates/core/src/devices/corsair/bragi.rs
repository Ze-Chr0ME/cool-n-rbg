//! Corsair "V2"/Bragi peripherals in software lighting mode (Harpoon RGB Wireless over USB).
//! See docs/PROTOCOLS.md.
use super::{valid_dpi, MouseChange, MouseState, DPI_STAGES, POLLING_HZ};
use crate::effects::Rgb;
use crate::ipc::DeviceKind;
use hidapi::{HidApi, HidDevice};

/// (pid, name, led count, kind) of supported devices; LEDs are written as planar R.., G.., B...
pub const DEVICES: [(u16, &str, usize, DeviceKind); 1] = [(0x1b5e, "Corsair Harpoon RGB Wireless", 2, DeviceKind::Mouse)];

pub struct CorsairV2 {
    dev: HidDevice,
    pid: u16,
    prefix: u8,
    light_ctrl: u8,
    pub name: &'static str,
    pub kind: DeviceKind,
    pub leds: usize,
    /// Switch back to hardware lighting when dropped.
    pub restore_on_drop: bool,
}

impl CorsairV2 {
    pub fn open_all(api: &HidApi) -> Vec<Result<CorsairV2, String>> {
        let mut out = Vec::new();
        for &(pid, name, leds, kind) in &DEVICES {
            for info in api.device_list().filter(|d| {
                d.vendor_id() == 0x1b1c && d.product_id() == pid && d.usage_page() == 0xff42 && d.usage() == 1
            }) {
                out.push(
                    info.open_device(api)
                        .map_err(|e| format!("{name}: {e}"))
                        .and_then(|dev| CorsairV2::init(dev, pid, 0x08, name, leds, kind)),
                );
            }
        }
        out
    }

    fn xfer(&self, data: &[u8], read: bool) -> Result<[u8; 64], String> {
        let mut w = [0u8; 65];
        w[1] = self.prefix;
        w[2..2 + data.len()].copy_from_slice(data);
        self.dev.write(&w).map_err(|e| format!("{}: write {e}", self.name))?;
        let mut r = [0u8; 64];
        if read {
            self.dev.read_timeout(&mut r, 50).map_err(|e| format!("{}: read {e}", self.name))?;
        }
        Ok(r)
    }

    fn drain(&self) {
        let mut r = [0u8; 64];
        while matches!(self.dev.read_timeout(&mut r, 3), Ok(n) if n > 0) {}
    }

    fn init(dev: HidDevice, pid: u16, prefix: u8, name: &'static str, leds: usize, kind: DeviceKind) -> Result<CorsairV2, String> {
        let mut d = CorsairV2 { dev, pid, prefix, light_ctrl: 0x22, name, kind, leds, restore_on_drop: true };
        d.drain();
        // Software render mode, then lighting control init (OpenRGB SW controller sequence).
        d.xfer(&[0x01, 0x03, 0x00, 0x02], true)?;
        d.xfer(&[0x02, 0x5f], true)?;
        // Pick the lighting endpoint the device accepts.
        if d.xfer(&[0x0d, 0x00, d.light_ctrl], true)?[2] != 0 {
            d.light_ctrl = 0x01;
            d.xfer(&[0x0d, 0x00, d.light_ctrl], true)?;
        }
        d.xfer(&[0x05, 0x01, 0x00], true)?;
        Ok(d)
    }

    pub fn set_colors(&self, colors: &[Rgb]) -> Result<(), String> {
        let n = colors.len();
        let mut data = vec![0u8; n * 3];
        for (i, c) in colors.iter().enumerate() {
            data[i] = c.0;
            data[n + i] = c.1;
            data[2 * n + i] = c.2;
        }
        self.drain();
        self.xfer(&[0x0d, 0x00, self.light_ctrl], true)?;
        // First block: [06 00 len_lo len_hi 00 00] then data from offset 8 of the report.
        let mut first = vec![0x06, 0x00, data.len() as u8, (data.len() >> 8) as u8, 0, 0];
        let take = data.len().min(65 - 8);
        first.extend_from_slice(&data[..take]);
        let r = self.xfer(&first, true)?;
        if r[2] != 0 {
            return Err(format!("{}: color write status {:02x}", self.name, r[2]));
        }
        for part in data[take..].chunks(65 - 4) {
            let mut more = vec![0x07, 0x00];
            more.extend_from_slice(part);
            self.xfer(&more, false)?;
        }
        self.xfer(&[0x05, 0x01, 0x00], true)?;
        Ok(())
    }

    pub fn hardware_mode(&self) {
        let _ = self.xfer(&[0x01, 0x03, 0x00, 0x01], true);
    }

    fn get(&self, prop: u8) -> Result<u32, String> {
        let r = self.xfer(&[0x02, prop], true)?;
        if r[1] != 0x02 || r[2] != 0 {
            return Err(format!("{}: read {prop:#04x} status {:02x}", self.name, r[2]));
        }
        Ok(u32::from_le_bytes([r[3], r[4], r[5], r[6]]))
    }

    /// Only accepted in software mode; hardware mode answers status 03.
    fn set(&self, prop: u8, value: u32) -> Result<(), String> {
        let v = value.to_le_bytes();
        let r = self.xfer(&[0x01, prop, 0x00, v[0], v[1], v[2], v[3]], true)?;
        if r[2] != 0 {
            return Err(format!("{}: write {prop:#04x}={value} status {:02x}", self.name, r[2]));
        }
        Ok(())
    }

    /// The mouse's button event interface (usage 2): `[1]=02 [2]=pressed button bits`, DPI button = `0x08`.
    pub fn open_events(&self, api: &HidApi) -> Option<HidDevice> {
        let info = api.device_list().find(|d| {
            d.vendor_id() == 0x1b1c && d.product_id() == self.pid && d.usage_page() == 0xff42 && d.usage() == 2
        })?;
        info.open_device(api).ok()
    }

    pub fn mouse_state(&self, id: &str) -> MouseState {
        self.drain();
        let mut s = MouseState { id: id.into(), ..Default::default() };
        let mut read = || -> Result<(), String> {
            s.dpi_stages = (0..DPI_STAGES as u8).map(|i| self.get(PROP_DPI_STAGE0 + i).map(|v| v as u16)).collect::<Result<_, _>>()?;
            s.stage = self.get(PROP_DPI_INDEX)? as u8;
            s.polling_hz = POLLING_HZ.get((self.get(PROP_POLLING)? as usize).wrapping_sub(1)).copied();
            s.angle_snapping = Some(self.get(PROP_ANGLE_SNAP)? != 0);
            s.sleep_minutes = Some((self.get(PROP_SLEEP)? / 60_000) as u16);
            s.battery = Some((self.get(PROP_BATTERY)? / 10).min(100) as u8);
            Ok(())
        };
        if let Err(e) = read() {
            s.error = Some(e);
        }
        s
    }

    pub fn apply(&mut self, change: &MouseChange) -> Result<(), String> {
        self.drain();
        match change {
            MouseChange::DpiStages { dpi } => {
                let active = self.get(PROP_DPI_INDEX)? as usize;
                for (i, &d) in dpi.iter().take(DPI_STAGES).enumerate() {
                    let d = valid_dpi(d) as u32;
                    if self.get(PROP_DPI_STAGE0 + i as u8)? != d {
                        self.set(PROP_DPI_STAGE0 + i as u8, d)?;
                    }
                    if i == active {
                        self.set(PROP_DPI, d)?;
                    }
                }
            }
            MouseChange::Stage { stage } => self.select_stage(*stage as u32 % DPI_STAGES as u32)?,
            MouseChange::NextStage => self.select_stage((self.get(PROP_DPI_INDEX)? + 1) % DPI_STAGES as u32)?,
            MouseChange::PollingRate { hz } => {
                let code = POLLING_HZ.iter().position(|p| p == hz).ok_or(format!("{}: no {hz} Hz polling", self.name))?;
                self.set(PROP_POLLING, code as u32 + 1)?;
            }
            MouseChange::AngleSnapping { on } => self.set(PROP_ANGLE_SNAP, *on as u32)?,
            MouseChange::Sleep { minutes } => {
                // OpenLinkHub's order: enable idle mode, then both timeout registers.
                let ms = *minutes as u32 * 60_000;
                self.set(PROP_IDLE_MODE, 1)?;
                self.set(PROP_SLEEP2, ms)?;
                self.set(PROP_SLEEP, ms)?;
            }
        }
        Ok(())
    }

    /// The active stage is stored on the mouse, so hardware mode keeps it once the daemon is gone.
    fn select_stage(&self, stage: u32) -> Result<(), String> {
        let dpi = self.get(PROP_DPI_STAGE0 + stage as u8)?;
        self.set(PROP_DPI_INDEX, stage)?;
        self.set(PROP_DPI, dpi)
    }
}

// Bragi properties, see docs/PROTOCOLS.md.
const PROP_POLLING: u8 = 0x01;
const PROP_ANGLE_SNAP: u8 = 0x07;
const PROP_IDLE_MODE: u8 = 0x0d;
const PROP_SLEEP: u8 = 0x0e;
const PROP_BATTERY: u8 = 0x0f;
const PROP_DPI_STAGE0: u8 = 0x18;
const PROP_DPI_INDEX: u8 = 0x1e;
const PROP_DPI: u8 = 0x20;
const PROP_SLEEP2: u8 = 0x37;

impl Drop for CorsairV2 {
    fn drop(&mut self) {
        if self.restore_on_drop {
            self.hardware_mode();
        }
    }
}
