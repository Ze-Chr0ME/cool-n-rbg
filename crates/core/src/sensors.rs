//! CPU (AMD Zen Tctl via PawnIO) and NVIDIA GPU (NVML) temperatures, board name.
use crate::pawnio::{blobs, GlobalMutex, Module};
use std::ffi::c_void;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
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

type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type NvmlTemp = unsafe extern "C" fn(*mut c_void, i32, *mut u32) -> i32;

pub struct GpuTemp {
    dev: *mut c_void,
    temp: NvmlTemp,
}

unsafe impl Send for GpuTemp {}

impl GpuTemp {
    pub fn open() -> Result<GpuTemp, String> {
        unsafe {
            let m = LoadLibraryW(wide("nvml.dll").as_ptr());
            if m.is_null() {
                return Err("nvml.dll not found".into());
            }
            let sym = |n: &[u8]| GetProcAddress(m, n.as_ptr()).ok_or("nvml: missing export".to_string());
            let init: NvmlInit = std::mem::transmute(sym(b"nvmlInit_v2\0")?);
            let handle: NvmlHandle = std::mem::transmute(sym(b"nvmlDeviceGetHandleByIndex_v2\0")?);
            let temp: NvmlTemp = std::mem::transmute(sym(b"nvmlDeviceGetTemperature\0")?);
            if init() != 0 {
                return Err("nvmlInit failed".into());
            }
            let mut dev = std::ptr::null_mut();
            if handle(0, &mut dev) != 0 {
                return Err("no NVIDIA GPU".into());
            }
            Ok(GpuTemp { dev, temp })
        }
    }

    pub fn read(&self) -> Option<f32> {
        let mut t = 0u32;
        (unsafe { (self.temp)(self.dev, 0, &mut t) } == 0).then_some(t as f32)
    }
}
