//! NVIDIA GPU temperature and fan control through nvapi64.dll (the private "client fan coolers"
//! interface also used by LibreHardwareMonitor / FanControl). Struct layouts and interface ids are
//! from LibreHardwareMonitor's NvApi.cs.
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const MAX_ITEMS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
struct Sensor {
    controller: i32,
    default_min: u32,
    default_max: u32,
    current: u32,
    target: i32,
}

#[repr(C)]
struct ThermalSettings {
    version: u32,
    count: u32,
    sensors: [Sensor; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CoolerStatusItem {
    pub cooler_id: u32,
    pub rpm: u32,
    pub min_level: u32,
    pub max_level: u32,
    pub level: u32,
    reserved: [u32; 8],
}

#[repr(C)]
struct CoolersStatus {
    version: u32,
    count: u32,
    reserved: [u64; 4],
    items: [CoolerStatusItem; MAX_ITEMS],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ControlItem {
    cooler_id: u32,
    level: u32,
    /// 0 = auto (driver/VBIOS curve), 1 = manual.
    mode: u32,
    reserved: [u32; 8],
}

#[repr(C)]
struct CoolerControl {
    version: u32,
    reserved: u32,
    count: u32,
    reserved2: [u32; 8],
    items: [ControlItem; MAX_ITEMS],
}

const fn version<T>(v: u32) -> u32 {
    std::mem::size_of::<T>() as u32 | (v << 16)
}

type QueryFn = unsafe extern "C" fn(u32) -> *mut c_void;
type InitFn = unsafe extern "C" fn() -> i32;
type EnumFn = unsafe extern "C" fn(*mut *mut c_void, *mut i32) -> i32;
type ThermalFn = unsafe extern "C" fn(*mut c_void, i32, *mut ThermalSettings) -> i32;
type StatusFn = unsafe extern "C" fn(*mut c_void, *mut CoolersStatus) -> i32;
type ControlFn = unsafe extern "C" fn(*mut c_void, *mut CoolerControl) -> i32;

pub struct NvGpu {
    lib: HMODULE,
    gpu: *mut c_void,
    thermal: ThermalFn,
    status: StatusFn,
    get_control: ControlFn,
    set_control: ControlFn,
    unload: InitFn,
}

unsafe impl Send for NvGpu {}

#[derive(Clone, Debug, Default)]
pub struct Cooler {
    pub id: u32,
    pub rpm: u32,
    pub percent: u32,
    pub min_percent: u32,
    pub max_percent: u32,
    pub manual: bool,
}

impl NvGpu {
    /// First NVIDIA GPU.
    pub fn open() -> Result<NvGpu, String> {
        unsafe {
            let name: Vec<u16> = "nvapi64.dll\0".encode_utf16().collect();
            let lib = LoadLibraryW(name.as_ptr());
            if lib.is_null() {
                return Err("nvapi64.dll not found".into());
            }
            let Some(q) = GetProcAddress(lib, b"nvapi_QueryInterface\0".as_ptr()) else {
                FreeLibrary(lib);
                return Err("nvapi: no QueryInterface".into());
            };
            let q: QueryFn = std::mem::transmute(q);
            let get = |id: u32| {
                let p = q(id);
                (!p.is_null()).then_some(p).ok_or(format!("nvapi: interface {id:#x} missing"))
            };
            let init: InitFn = std::mem::transmute(get(0x0150_E828)?);
            if init() != 0 {
                FreeLibrary(lib);
                return Err("NvAPI_Initialize failed".into());
            }
            let enumerate: EnumFn = std::mem::transmute(get(0xE5AC_921F)?);
            let mut gpu = NvGpu {
                lib,
                gpu: std::ptr::null_mut(),
                thermal: std::mem::transmute(get(0xE364_0A56)?),
                status: std::mem::transmute(get(0x35AE_D5E8)?),
                get_control: std::mem::transmute(get(0x814B_209F)?),
                set_control: std::mem::transmute(get(0xA589_71A5)?),
                unload: std::mem::transmute(get(0xD22B_DD7E)?),
            };
            let mut handles = [std::ptr::null_mut(); 64];
            let mut n = 0;
            if enumerate(handles.as_mut_ptr(), &mut n) != 0 || n < 1 {
                return Err("no NVIDIA GPU".into());
            }
            gpu.gpu = handles[0];
            Ok(gpu)
        }
    }

    /// GPU core temperature in °C.
    pub fn temperature(&self) -> Option<f32> {
        let mut t: ThermalSettings = unsafe { std::mem::zeroed() };
        t.version = version::<ThermalSettings>(2);
        let ok = unsafe { (self.thermal)(self.gpu, 15 /* all sensors */, &mut t) } == 0;
        (ok && t.count > 0).then(|| t.sensors[0].current as f32)
    }

    fn read_control(&self) -> Result<CoolerControl, String> {
        let mut c: CoolerControl = unsafe { std::mem::zeroed() };
        c.version = version::<CoolerControl>(1);
        match unsafe { (self.get_control)(self.gpu, &mut c) } {
            0 => Ok(c),
            s => Err(format!("NvAPI ClientFanCoolersGetControl: {s}")),
        }
    }

    pub fn coolers(&self) -> Result<Vec<Cooler>, String> {
        let mut s: CoolersStatus = unsafe { std::mem::zeroed() };
        s.version = version::<CoolersStatus>(1);
        let r = unsafe { (self.status)(self.gpu, &mut s) };
        if r != 0 {
            return Err(format!("NvAPI ClientFanCoolersGetStatus: {r}"));
        }
        let control = self.read_control().ok();
        Ok(s.items[..(s.count as usize).min(MAX_ITEMS)]
            .iter()
            .map(|it| Cooler {
                id: it.cooler_id,
                rpm: it.rpm,
                percent: it.level,
                min_percent: it.min_level,
                max_percent: it.max_level,
                manual: control.as_ref().is_some_and(|c| {
                    c.items[..(c.count as usize).min(MAX_ITEMS)].iter().any(|i| i.cooler_id == it.cooler_id && i.mode == 1)
                }),
            })
            .collect())
    }

    /// `Some(percent)` = manual duty on every fan, `None` = back to the card's own automatic curve.
    pub fn set_all(&self, percent: Option<u32>) -> Result<(), String> {
        let mut c = self.read_control()?;
        for it in c.items[..(c.count as usize).min(MAX_ITEMS)].iter_mut() {
            match percent {
                Some(p) => {
                    it.mode = 1;
                    it.level = p.min(100);
                }
                None => {
                    it.mode = 0;
                    it.level = 0;
                }
            }
        }
        match unsafe { (self.set_control)(self.gpu, &mut c) } {
            0 => Ok(()),
            s => Err(format!("NvAPI ClientFanCoolersSetControl: {s}")),
        }
    }
}

impl Drop for NvGpu {
    fn drop(&mut self) {
        unsafe {
            (self.unload)();
            FreeLibrary(self.lib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_nvapi() {
        assert_eq!(std::mem::size_of::<ThermalSettings>(), 68);
        assert_eq!(std::mem::size_of::<CoolersStatus>(), 1704);
        assert_eq!(std::mem::size_of::<CoolerControl>(), 1452);
    }
}
