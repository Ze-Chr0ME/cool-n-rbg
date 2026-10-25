//! Corsair "V2"/Bragi peripherals in software lighting mode (Harpoon RGB Wireless over USB).
//! See docs/PROTOCOLS.md.
use crate::effects::Rgb;
use hidapi::{HidApi, HidDevice};

/// (pid, name, led count) of supported devices; LEDs are written as planar R.., G.., B...
pub const DEVICES: [(u16, &str, usize); 1] = [(0x1b5e, "Corsair Harpoon RGB Wireless", 2)];

pub struct CorsairV2 {
    dev: HidDevice,
    prefix: u8,
    light_ctrl: u8,
    pub name: &'static str,
    pub leds: usize,
}

impl CorsairV2 {
    pub fn open_all(api: &HidApi) -> Vec<Result<CorsairV2, String>> {
        let mut out = Vec::new();
        for &(pid, name, leds) in &DEVICES {
            for info in api.device_list().filter(|d| {
                d.vendor_id() == 0x1b1c && d.product_id() == pid && d.usage_page() == 0xff42 && d.usage() == 1
            }) {
                out.push(
                    info.open_device(api)
                        .map_err(|e| format!("{name}: {e}"))
                        .and_then(|dev| CorsairV2::init(dev, 0x08, name, leds)),
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

    fn init(dev: HidDevice, prefix: u8, name: &'static str, leds: usize) -> Result<CorsairV2, String> {
        let mut d = CorsairV2 { dev, prefix, light_ctrl: 0x22, name, leds };
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
}

impl Drop for CorsairV2 {
    fn drop(&mut self) {
        self.hardware_mode();
    }
}
