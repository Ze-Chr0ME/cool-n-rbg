//! Corsair iCUE Commander Core (1B1C:0C32 and siblings). See docs/PROTOCOLS.md.
use crate::config::{MIN_FAN_PERCENT, MIN_PUMP_PERCENT};
use crate::effects::Rgb;
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

pub const PIDS: [u16; 3] = [0x0c32, 0x0c1c, 0x0c1d];
const CHANNELS: usize = 7;

const CMD_SOFTWARE: [u8; 4] = [0x01, 0x03, 0x00, 0x02];
const CMD_HARDWARE: [u8; 4] = [0x01, 0x03, 0x00, 0x01];
const EP_LEDS: u8 = 0x20;
const EP_FANS: u8 = 0x1a;
const EP_SPEEDS: u8 = 0x17;
const EP_TEMPS: u8 = 0x21;
const EP_SET_SPEED: u8 = 0x18;
const EP_COLOR: u8 = 0x22;

pub struct CommanderCore {
    dev: HidDevice,
    /// Packet size without the report id byte (64 or 96).
    packet: usize,
    pub firmware: String,
    /// LED count per port (0 = pump), 0 when nothing is connected.
    pub port_leds: [u16; CHANNELS],
    /// Fan/pump present per channel.
    pub connected: [bool; CHANNELS],
    last_color_write: Instant,
    /// Switch back to the stored hardware profile when dropped.
    pub restore_on_drop: bool,
}

impl CommanderCore {
    pub fn open(api: &HidApi) -> Option<Result<CommanderCore, String>> {
        let info = api.device_list().find(|d| {
            d.vendor_id() == 0x1b1c && PIDS.contains(&d.product_id()) && d.interface_number() == 0
        })?;
        let pid = info.product_id();
        Some(info.open_device(api).map_err(|e| e.to_string()).and_then(|dev| {
            let mut cc = CommanderCore {
                dev,
                packet: if pid == 0x0c1c { 96 } else { 64 },
                firmware: String::new(),
                port_leds: [0; CHANNELS],
                connected: [false; CHANNELS],
                last_color_write: Instant::now() - Duration::from_secs(60),
                restore_on_drop: true,
            };
            cc.init()?;
            Ok(cc)
        }))
    }

    fn xfer(&self, cmd: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
        let mut w = vec![0u8; self.packet + 1];
        w[1] = 0x08;
        w[2..2 + cmd.len()].copy_from_slice(cmd);
        let end = (2 + cmd.len() + data.len()).min(w.len());
        w[2 + cmd.len()..end].copy_from_slice(&data[..end - 2 - cmd.len()]);
        self.dev.write(&w).map_err(|e| format!("commander core write: {e}"))?;
        let mut r = vec![0u8; self.packet];
        // Skip unsolicited or stale reports until the echo of our command arrives.
        for _ in 0..4 {
            let n = self.dev.read_timeout(&mut r, 500).map_err(|e| format!("commander core read: {e}"))?;
            if n == 0 {
                return Err("commander core: timeout".into());
            }
            if r[0] == 0x00 && r[1] == cmd[0] {
                return Ok(r);
            }
        }
        Err("commander core: unexpected response".into())
    }

    fn read_ep(&self, ep: u8) -> Result<Vec<u8>, String> {
        self.xfer(&[0x05, 0x01, 0x01], &[ep])?;
        self.xfer(&[0x0d, 0x01], &[ep])?;
        let r = self.xfer(&[0x08, 0x01], &[ep])?;
        self.xfer(&[0x05, 0x01, 0x01], &[ep])?;
        if r[2] != 0 {
            return Err(format!("commander core: read endpoint {ep:02x} status {:02x}", r[2]));
        }
        Ok(r)
    }

    fn write_ep(&self, ep: u8, dtype: &[u8], data: &[u8], extra: bool) -> Result<(), String> {
        let len = data.len() as u16 + if extra { 2 } else { 0 };
        let mut buf = vec![len as u8, (len >> 8) as u8, 0, 0];
        buf.extend_from_slice(dtype);
        buf.extend_from_slice(data);
        self.xfer(&[0x05, 0x01, 0x01], &[ep])?;
        self.xfer(&[0x0d, 0x01], &[ep])?;
        self.xfer(&[0x06, 0x01], &buf)?;
        self.xfer(&[0x05, 0x01, 0x01], &[ep])?;
        Ok(())
    }

    fn init(&mut self) -> Result<(), String> {
        let fw = self.xfer(&[0x02, 0x13], &[])?;
        self.firmware = format!("{}.{}.{}", fw[3], fw[4], u16::from_le_bytes([fw[5], fw[6]]));
        self.xfer(&CMD_SOFTWARE, &[])?;

        let leds = self.read_ep(EP_LEDS)?;
        let count = (leds[5] as usize).min(CHANNELS);
        for i in 0..count {
            let o = 6 + i * 4;
            if leds[o] == 2 {
                self.port_leds[i] = u16::from_le_bytes([leds[o + 2], leds[o + 3]]);
            }
        }
        let fans = self.read_ep(EP_FANS)?;
        for i in 0..(fans[5] as usize).min(CHANNELS) {
            self.connected[i] = matches!(fans[6 + i], 0x07 | 0x02);
        }

        // Keep the color endpoint open on slot 0 for the session.
        self.xfer(&[0x05, 0x01, 0x01], &[EP_COLOR])?;
        self.xfer(&[0x0d, 0x00], &[EP_COLOR])?;
        Ok(())
    }

    /// The hub keeps the LED port layout configured by iCUE; colors are sent for every LED, ports in order.
    pub fn led_count(&self) -> usize {
        self.port_leds.iter().map(|&n| n as usize).sum()
    }

    pub fn set_colors(&mut self, colors: &[Rgb]) -> Result<(), String> {
        let mut data = Vec::with_capacity(colors.len() * 3);
        for c in colors {
            data.extend_from_slice(&[c.0, c.1, c.2]);
        }
        let len = data.len() as u16 + 2;
        let mut buf = vec![len as u8, (len >> 8) as u8, 0, 0, 0x12, 0x00];
        buf.extend_from_slice(&data);
        let chunk = self.packet - 3;
        for (i, part) in buf.chunks(chunk).enumerate() {
            let cmd: &[u8] = if i == 0 { &[0x06, 0x00] } else { &[0x07, 0x00] };
            let r = self.xfer(cmd, part)?;
            if r[2] != 0 {
                return Err(format!("commander core: color write status {:02x}", r[2]));
            }
        }
        self.last_color_write = Instant::now();
        Ok(())
    }

    pub fn since_last_color(&self) -> Duration {
        self.last_color_write.elapsed()
    }

    /// RPM per channel (index 0 = pump) and liquid temperature.
    pub fn sensors(&self) -> Result<([u16; CHANNELS], Option<f32>), String> {
        let s = self.read_ep(EP_SPEEDS)?;
        let mut rpm = [0u16; CHANNELS];
        for (i, r) in rpm.iter_mut().enumerate().take((s[5] as usize).min(CHANNELS)) {
            *r = u16::from_le_bytes([s[6 + i * 2], s[7 + i * 2]]);
        }
        let t = self.read_ep(EP_TEMPS)?;
        let liquid = (t[5] > 0 && t[6] == 0x00)
            .then(|| i16::from_le_bytes([t[7], t[8]]) as f32 / 10.0)
            .filter(|v| (5.0..90.0).contains(v));
        Ok((rpm, liquid))
    }

    /// Set duty percent per channel (index 0 = pump). Floors are enforced here.
    pub fn set_duty(&self, duty: &[u8; CHANNELS]) -> Result<(), String> {
        let chans: Vec<usize> = (0..CHANNELS).filter(|&i| self.connected[i]).collect();
        let mut buf = vec![chans.len() as u8];
        for &i in &chans {
            let floor = if i == 0 { MIN_PUMP_PERCENT } else { MIN_FAN_PERCENT };
            buf.extend_from_slice(&[i as u8, 0x00, duty[i].clamp(floor, 100), 0x00]);
        }
        self.write_ep(EP_SET_SPEED, &[0x07, 0x00], &buf, true)
    }

    /// Hand control back to the hub's stored (iCUE) hardware profile.
    pub fn hardware_mode(&self) {
        let _ = self.xfer(&CMD_HARDWARE, &[]);
    }
}

impl Drop for CommanderCore {
    fn drop(&mut self) {
        if self.restore_on_drop {
            self.hardware_mode();
        }
    }
}
