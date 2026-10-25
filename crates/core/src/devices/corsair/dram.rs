//! Corsair Vengeance RGB Pro DDR4 lighting over SMBus (direct, volatile mode only).
use crate::devices::transport::smbus::{crc8, Smbus};
use crate::effects::Rgb;
use std::sync::Arc;

pub const LEDS: usize = 10;

pub struct CorsairDram {
    bus: Arc<Smbus>,
    pub addr: u8,
    pub pid: u16,
    pub protocol: u8,
}

impl CorsairDram {
    /// Scan only the Corsair RGB controller range. 0x50-0x57 (SPD EEPROM) is never touched.
    pub fn detect(bus: &Arc<Smbus>) -> Vec<CorsairDram> {
        let mut found = Vec::new();
        for addr in 0x58..=0x5Fu8 {
            let id = bus.read_byte(addr, 0x43);
            let ver = bus.read_byte(addr, 0x44);
            if matches!(id, Some(0x1a..=0x1c)) && matches!(ver, Some(0x01 | 0x03 | 0x04)) {
                let mut d = CorsairDram { bus: bus.clone(), addr, pid: 0, protocol: 0 };
                d.read_info();
                found.push(d);
            }
        }
        found
    }

    fn read_info(&mut self) {
        self.bus.write_byte(self.addr, 0x61, 0x00);
        self.bus.write_byte(self.addr, 0x21, 0x00);
        let mut info = [0u8; 32];
        for b in info.iter_mut() {
            *b = self.bus.read_byte(self.addr, 0x40).unwrap_or(0);
        }
        if self.bus.read_byte(self.addr, 0x42) == Some(crc8(&info)) {
            self.pid = u16::from_le_bytes([info[2], info[3]]);
            self.protocol = info[28];
        }
    }

    pub fn supports_direct(&self) -> bool {
        self.protocol >= 4
    }

    pub fn set_colors(&self, colors: &[Rgb]) -> Result<(), String> {
        let mut pkt = [0u8; LEDS * 3 + 2];
        pkt[0] = LEDS as u8;
        for (i, c) in colors.iter().take(LEDS).enumerate() {
            pkt[1 + i * 3..4 + i * 3].copy_from_slice(&[c.0, c.1, c.2]);
        }
        pkt[LEDS * 3 + 1] = crc8(&pkt[..LEDS * 3 + 1]);
        if self.bus.write_block(self.addr, 0x31, &pkt) {
            Ok(())
        } else {
            Err(format!("DRAM {:#04x}: block write failed", self.addr))
        }
    }
}
