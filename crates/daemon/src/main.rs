//! coolrgbd: background RGB + fan service with a tray icon. No console, no GUI toolkit.
#![windows_subsystem = "windows"]

mod autostart;
mod engine;
mod pipe;

use coolrgb_core::config::Config;
use coolrgb_core::ipc::Request;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_QUIT_REQUEST: u32 = WM_APP + 2;
const ID_OPEN: usize = 1;
const ID_RESCAN: usize = 2;
const ID_EXIT: usize = 3;

static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);
static mut SHARED: Option<std::sync::Arc<engine::Shared>> = None;
static mut THREADS: Vec<std::thread::JoinHandle<()>> = Vec::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Private bytes of this process in KiB (what Task Manager's "Memory" column tracks).
pub fn working_set_kb() -> u64 {
    let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), c.cb) };
    c.PrivateUsage as u64 / 1024
}

/// 32x32 rainbow ring icon generated at runtime (no resource compiler needed).
fn make_icon() -> HICON {
    const N: i32 = 32;
    let mut bgra = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let r = (dx * dx + dy * dy).sqrt();
            if (8.0..15.5).contains(&r) {
                let c = coolrgb_core::effects::Rgb::from_hue(dy.atan2(dx) / std::f32::consts::TAU);
                let i = ((y * N + x) * 4) as usize;
                bgra[i..i + 4].copy_from_slice(&[c.2, c.1, c.0, 255]);
            }
        }
    }
    let mask = vec![0u8; (N * N / 8) as usize];
    unsafe {
        let color = CreateBitmap(N, N, 1, 32, bgra.as_ptr().cast());
        let mask = CreateBitmap(N, N, 1, 1, mask.as_ptr().cast());
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid
}

fn add_tray(hwnd: HWND) {
    let mut nid = tray_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = make_icon();
    let tip = wide("Cool'n RGB");
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
}

fn open_gui() {
    if let Ok(exe) = std::env::current_exe() {
        let gui = exe.with_file_name("coolrgb.exe");
        if let Err(e) = std::process::Command::new(&gui).spawn() {
            engine::log(&format!("starting {}: {e}", gui.display()));
        }
    }
}

fn shutdown() {
    unsafe {
        #[allow(static_mut_refs)]
        if let Some(sh) = SHARED.as_ref() {
            sh.stop.store(true, Ordering::Relaxed);
            #[allow(static_mut_refs)]
            for t in THREADS.drain(..) {
                let _ = t.join();
            }
        }
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Open Cool'n RGB").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_RESCAN, wide("Rescan devices").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT, wide("Exit").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        DestroyMenu(menu);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg as isize == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
        add_tray(hwnd);
        return 0;
    }
    match msg {
        WM_TRAY => {
            match lp as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => open_gui(),
                WM_RBUTTONUP => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match wp & 0xffff {
                ID_OPEN => open_gui(),
                ID_RESCAN => {
                    #[allow(static_mut_refs)]
                    if let Some(sh) = SHARED.as_ref() {
                        sh.rescan_gen.fetch_add(1, Ordering::Relaxed);
                    }
                }
                ID_EXIT => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_QUIT_REQUEST => {
            DestroyWindow(hwnd);
            0
        }
        // Resume from sleep: USB devices re-enumerate, reopen everything.
        WM_POWERBROADCAST if wp as u32 == PBT_APMRESUMEAUTOMATIC => {
            #[allow(static_mut_refs)]
            if let Some(sh) = SHARED.as_ref() {
                sh.rescan_gen.fetch_add(1, Ordering::Relaxed);
            }
            1
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wp != 0 {
                shutdown();
            }
            0
        }
        WM_DESTROY => {
            Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    let single = wide("Local\\coolrgbd-single-instance");
    unsafe {
        CreateMutexW(std::ptr::null(), 0, single.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }
    let sh = engine::Shared::new(Config::load());
    unsafe {
        SHARED = Some(sh.clone());
        THREADS = engine::spawn(sh.clone());
    }
    pipe::serve(sh.clone(), |req| {
        if matches!(req, Request::Quit | Request::Handover) {
            let h = HWND_MAIN.load(Ordering::Relaxed);
            unsafe { PostMessageW(h as HWND, WM_QUIT_REQUEST, 0, 0) };
        }
    });
    engine::log(&format!("started, pid {}", std::process::id()));

    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("coolrgbd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) as isize, Ordering::Relaxed);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Cool'n RGB").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        HWND_MAIN.store(hwnd as isize, Ordering::Relaxed);
        add_tray(hwnd);
        if std::env::args().any(|a| a == "--open") {
            open_gui();
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    shutdown();
    engine::log("stopped");
}
