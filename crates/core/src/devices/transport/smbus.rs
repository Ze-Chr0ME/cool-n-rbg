//! AMD PIIX4-compatible SMBus through the PawnIO SmbusPIIX4 module.
use crate::pawnio::{blobs, GlobalMutex, Module};

const READ: u64 = 1;
const WRITE: u64 = 0;
const BYTE_DATA: u64 = 2;
const BLOCK_DATA: u64 = 5;

pub struct Smbus {
    m: Module,
    lock: GlobalMutex,
}

impl Smbus {
    pub fn open() -> Result<Smbus, String> {
        let m = Module::load(blobs::SMBUS_PIIX4)?;
        let s = Smbus { m, lock: GlobalMutex::new("Global\\Access_SMBUS.HTP.Method") };
        // Main port 0 is where DIMMs live; this is also the chipset default.
        s.lock.with(|| s.m.exec("ioctl_piix4_port_sel", &[0], 1)).map_err(|e| format!("smbus port select: {e:#x}"))?;
        Ok(s)
    }

    pub fn read_byte(&self, addr: u8, cmd: u8) -> Option<u8> {
        self.lock
            .with(|| self.m.exec("ioctl_smbus_xfer", &[addr as u64, READ, cmd as u64, BYTE_DATA], 1))
            .ok()
            .and_then(|o| o.first().map(|&v| v as u8))
    }

    pub fn write_byte(&self, addr: u8, cmd: u8, val: u8) -> bool {
        self.lock
            .with(|| self.m.exec("ioctl_smbus_xfer", &[addr as u64, WRITE, cmd as u64, BYTE_DATA, val as u64], 0))
            .is_ok()
    }

    /// SMBus block write of up to 32 bytes.
    pub fn write_block(&self, addr: u8, cmd: u8, data: &[u8]) -> bool {
        let mut bytes = [0u8; 40];
        let n = data.len().min(32);
        bytes[0] = n as u8;
        bytes[1..1 + n].copy_from_slice(&data[..n]);
        let mut input = vec![addr as u64, WRITE, cmd as u64, BLOCK_DATA];
        input.extend(bytes.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())));
        self.lock.with(|| self.m.exec("ioctl_smbus_xfer", &input, 0)).is_ok()
    }
}

/// CRC-8, polynomial 0x07, init 0 (what Corsair DRAM controllers use).
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc8_check_value() {
        assert_eq!(super::crc8(b"123456789"), 0xF4);
    }
}
