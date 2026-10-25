//! Minimal dynamic binding to PawnIOLib.dll (installed by the PawnIO driver setup).
use std::ffi::{c_char, CString};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

type OpenFn = unsafe extern "system" fn(*mut HANDLE) -> i32;
type LoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> i32;
type ExecFn =
    unsafe extern "system" fn(HANDLE, *const c_char, *const u64, usize, *mut u64, usize, *mut usize) -> i32;
type CloseFn = unsafe extern "system" fn(HANDLE) -> i32;

struct Lib {
    open: OpenFn,
    load: LoadFn,
    exec: ExecFn,
    close: CloseFn,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
        let path = wide(&format!("{pf}\\PawnIO\\PawnIOLib.dll"));
        let m = LoadLibraryW(path.as_ptr());
        if m.is_null() {
            return Err("PawnIOLib.dll not found (is PawnIO installed?)".into());
        }
        let sym = |name: &[u8]| GetProcAddress(m, name.as_ptr()).ok_or_else(|| "PawnIOLib.dll: missing export".to_string());
        Ok(Lib {
            open: std::mem::transmute::<_, OpenFn>(sym(b"pawnio_open\0")?),
            load: std::mem::transmute::<_, LoadFn>(sym(b"pawnio_load\0")?),
            exec: std::mem::transmute::<_, ExecFn>(sym(b"pawnio_execute\0")?),
            close: std::mem::transmute::<_, CloseFn>(sym(b"pawnio_close\0")?),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// One loaded PawnIO module (e.g. SmbusPIIX4.bin).
pub struct Module {
    h: HANDLE,
}

unsafe impl Send for Module {}

impl Module {
    pub fn load(blob: &[u8]) -> Result<Module, String> {
        let l = lib()?;
        let mut h: HANDLE = std::ptr::null_mut();
        unsafe {
            let hr = (l.open)(&mut h);
            if hr < 0 {
                return Err(format!("pawnio_open failed: {hr:#x} (run as administrator)"));
            }
            let hr = (l.load)(h, blob.as_ptr(), blob.len());
            if hr < 0 {
                (l.close)(h);
                return Err(format!("pawnio_load failed: {hr:#x}"));
            }
        }
        Ok(Module { h })
    }

    pub fn exec(&self, name: &str, input: &[u64], out_len: usize) -> Result<Vec<u64>, i32> {
        let l = lib().map_err(|_| -1)?;
        let cname = CString::new(name).unwrap();
        let mut out = vec![0u64; out_len];
        let mut ret = 0usize;
        let hr = unsafe {
            (l.exec)(self.h, cname.as_ptr(), input.as_ptr(), input.len(), out.as_mut_ptr(), out.len(), &mut ret)
        };
        if hr < 0 {
            return Err(hr);
        }
        out.truncate(ret);
        Ok(out)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        if let Ok(l) = lib() {
            unsafe { (l.close)(self.h) };
        }
    }
}

/// Named global mutex shared with other hardware tools (LibreHardwareMonitor, OpenRGB, ...).
pub struct GlobalMutex(HANDLE);

unsafe impl Send for GlobalMutex {}

impl GlobalMutex {
    pub fn new(name: &str) -> GlobalMutex {
        let w = wide(name);
        GlobalMutex(unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) })
    }

    /// Runs `f` while holding the mutex (or without it if the mutex could not be created).
    pub fn with<T>(&self, f: impl FnOnce() -> T) -> T {
        let held = !self.0.is_null() && {
            let r = unsafe { WaitForSingleObject(self.0, 500) };
            r == WAIT_OBJECT_0 || r == WAIT_ABANDONED
        };
        let v = f();
        if held {
            unsafe { ReleaseMutex(self.0) };
        }
        v
    }
}

impl Drop for GlobalMutex {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Module blobs are embedded so the binaries are self-contained.
pub mod blobs {
    pub static SMBUS_PIIX4: &[u8] = include_bytes!("../../../vendor/pawnio/SmbusPIIX4.bin");
    pub static LPC_IO: &[u8] = include_bytes!("../../../vendor/pawnio/LpcIO.bin");
    pub static AMD_FAMILY17: &[u8] = include_bytes!("../../../vendor/pawnio/AMDFamily17.bin");
}
