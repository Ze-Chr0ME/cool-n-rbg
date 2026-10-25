//! CPU temperature (AMD Zen Tctl via PawnIO) and board name. GPU temperature lives in `nvapi`.
use crate::pawnio::{blobs, GlobalMutex, Module};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// e.g. "B450 TOMAHAWK (MS-7C02)".
pub fn board_name() -> String {
    let key = wide("HARDWARE\\DESCRIPTION\\System\\BIOS");
    let val = wide("BaseBoardProduct");
    let mut buf = [0u16; 128];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
    };
    if r != 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)])
}

pub struct CpuTemp {
    m: Module,
    lock: GlobalMutex,
}

impl CpuTemp {
    pub fn open() -> Result<CpuTemp, String> {
        let m = Module::load(blobs::AMD_FAMILY17)?;
        Ok(CpuTemp { m, lock: GlobalMutex::new("Global\\Access_PCI") })
    }

    /// Tctl in °C.
    pub fn read(&self) -> Option<f32> {
        let v = self.lock.with(|| self.m.exec("ioctl_read_smn", &[0x0005_9800], 1)).ok()?;
        let v = *v.first()? as u32;
        let mut t = ((v >> 21) & 0x7ff) as f32 / 8.0;
        if v & (1 << 19) != 0 {
            t -= 49.0;
        }
        (1.0..125.0).contains(&t).then_some(t)
    }
}
