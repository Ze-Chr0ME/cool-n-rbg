//! MSI-RGB (Nuvoton NCT6795D/NCT6797D Super-IO) onboard lighting, e.g. B450 TOMAHAWK.
//! Register sequence from OpenRGB MSIRGBController. Single zone, 4 bits per channel.
use crate::effects::Rgb;
use crate::pawnio::{blobs, GlobalMutex, Module};

/// Board DMI product names known to use MSI-RGB, with the color invert flag.
const BOARDS: [(&str, bool); 2] = [("MS-7C02", true), ("B450 TOMAHAWK", true)];
const REG_LOGDEV: u64 = 0x07;
const LOGDEV_RGB: u64 = 0x12;

pub struct MsiRgb {
    m: Module,
    lock: GlobalMutex,
    /// 0 = 0x2E/0x2F, 1 = 0x4E/0x4F.
    slot: u64,
    pub board: String,
}

impl MsiRgb {
    pub fn detect(board: &str) -> Result<Option<MsiRgb>, String> {
        let Some(&(_, invert)) = BOARDS.iter().find(|(name, _)| board.contains(name)) else {
            return Ok(None);
        };
        let m = Module::load(blobs::LPC_IO)?;
        let mut d = MsiRgb { m, lock: GlobalMutex::new(r"Global\Access_ISABUS.HTP.Method"), slot: 0, board: board.to_string() };
        let mut last = String::new();
        for slot in [0, 1] {
            d.slot = slot;
            match d.session(|d| {
                let id = (d.inb(0x20) << 8) | d.inb(0x21);
                if !matches!(id & 0xfff8, 0xd350 | 0xd450) {
                    return Err(format!("unexpected Super-IO id {id:#06x} in slot {slot}"));
                }
                Ok(())
            }) {
                Ok(()) => return d.setup(invert).map(|_| Some(d)),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn setup(&self, invert: bool) -> Result<(), String> {
        self.session(|d| {
            d.outb(REG_LOGDEV, 0x09);
            let v = (d.inb(0x2c) & 0b1111_0111) | 0b0001_0000;
            d.outb(0x2c, v);
            d.outb(REG_LOGDEV, LOGDEV_RGB);
            if d.inb(0xe0) & 0xe0 != 0xe0 {
                d.outb(0xe0, 0xe0);
            }
            d.outb(0xe4, 0x00);
            d.outb(0xff, if invert { 0b1111_1110 } else { 0b1110_0010 });
            d.outb(0xfd, 0x00);
            Ok(())
        })
    }

    fn inb(&self, reg: u64) -> u64 {
        match self.m.exec("ioctl_superio_inb", &[reg], 1) {
            Ok(o) => o.first().copied().unwrap_or(0xff),
            Err(e) => {
                eprintln!("superio_inb {reg:#x}: {e:#x}");
                0xff
            }
        }
    }

    fn outb(&self, reg: u64, val: u64) {
        let _ = self.m.exec("ioctl_superio_outb", &[reg, val], 0);
    }

    /// Enter Super-IO config mode, run `f`, leave config mode, all under the ISA bus mutex.
    fn session<T>(&self, f: impl FnOnce(&Self) -> Result<T, String>) -> Result<T, String> {
        self.lock.with(|| {
            let port = if self.slot == 0 { 0x2e } else { 0x4e };
            self.m.exec("ioctl_select_slot", &[self.slot], 0).map_err(|e| format!("select slot: {e:#x}"))?;
            self.m.exec("ioctl_pio_outb", &[port, 0x87], 0).map_err(|e| format!("sio enter: {e:#x}"))?;
            self.m.exec("ioctl_pio_outb", &[port, 0x87], 0).map_err(|e| format!("sio enter: {e:#x}"))?;
            let r = f(self);
            let _ = self.m.exec("ioctl_pio_outb", &[port, 0xaa], 0);
            r
        })
    }

    pub fn set_color(&self, c: Rgb) -> Result<(), String> {
        let nib = |v: u8| ((v >> 4) | (v & 0xf0)) as u64;
        self.session(|d| {
            d.outb(REG_LOGDEV, LOGDEV_RGB);
            for (base, v) in [(0xf0, c.0), (0xf4, c.1), (0xf8, c.2)] {
                for reg in base..base + 4 {
                    d.outb(reg, nib(v));
                }
            }
            Ok(())
        })
    }
}
