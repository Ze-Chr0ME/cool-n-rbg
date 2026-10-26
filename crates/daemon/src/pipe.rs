//! Named pipe server: one JSON request line in, one JSON response line out, per connection.
use crate::engine::{log, Shared};
use coolrgb_core::ipc::{Request, Response, PIPE_NAME};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `on_request` handles requests that need the UI thread (quit, autostart).
pub fn serve(sh: Arc<Shared>, on_request: impl Fn(&Request) + Send + Sync + 'static) {
    let on_request = Arc::new(on_request);
    std::thread::Builder::new()
        .name("pipe".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            // The daemon runs elevated; let any interactive (non-elevated) user process talk to it.
            let sddl = wide("D:(A;;GRGW;;;IU)(A;;GA;;;BA)(A;;GA;;;SY)");
            let mut sd = std::ptr::null_mut();
            unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut()) };
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            };
            let name = wide(PIPE_NAME);
            while !sh.stop.load(Ordering::Relaxed) {
                let h = unsafe {
                    CreateNamedPipeW(
                        name.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        64 * 1024,
                        64 * 1024,
                        0,
                        if sd.is_null() { std::ptr::null() } else { &sa },
                    )
                };
                if h == INVALID_HANDLE_VALUE {
                    log("CreateNamedPipe failed");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                let connected = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) } != 0
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                let file = unsafe { std::fs::File::from_raw_handle(h as _) };
                if !connected {
                    continue;
                }
                // One short-lived thread per client, so a stuck client can't block the next one.
                let (sh, on_request) = (sh.clone(), on_request.clone());
                let spawned = std::thread::Builder::new().stack_size(128 * 1024).spawn(move || {
                    handle(&sh, &file, &*on_request);
                    unsafe {
                        FlushFileBuffers(file.as_raw_handle() as _);
                        DisconnectNamedPipe(file.as_raw_handle() as _);
                    }
                });
                if spawned.is_err() {
                    log("pipe: could not spawn client thread");
                }
            }
            if !sd.is_null() {
                unsafe { LocalFree(sd) };
            }
        })
        .unwrap();
}

fn handle(sh: &Shared, file: &std::fs::File, on_request: &(impl Fn(&Request) + ?Sized)) {
    let mut line = String::new();
    if BufReader::new(file).read_line(&mut line).is_err() {
        return;
    }
    let resp = match serde_json::from_str::<Request>(&line) {
        Err(e) => Response::Error { message: format!("bad request: {e}") },
        Ok(req) => {
            let r = respond(sh, &req);
            on_request(&req);
            r
        }
    };
    let mut out = serde_json::to_string(&resp).unwrap();
    out.push('\n');
    let mut w = file;
    let _ = w.write_all(out.as_bytes());
}

fn respond(sh: &Shared, req: &Request) -> Response {
    match req {
        Request::Status => {
            let mut status = sh.status.lock().unwrap().clone();
            status.working_set_kb = crate::working_set_kb();
            Response::Status { status }
        }
        Request::GetConfig => Response::Config { config: sh.config.lock().unwrap().clone() },
        Request::SetConfig { config } => {
            let mut c = config.clone();
            c.brightness = c.brightness.min(100);
            let old_autostart = sh.config.lock().unwrap().autostart;
            if c.autostart != old_autostart {
                if let Err(e) = crate::autostart::set(c.autostart) {
                    log(&format!("autostart: {e}"));
                    c.autostart = old_autostart;
                }
            }
            if let Err(e) = c.save() {
                log(&format!("saving config: {e}"));
            }
            *sh.config.lock().unwrap() = c;
            sh.config_gen.fetch_add(1, Ordering::Release);
            Response::Ok
        }
        Request::Rescan => {
            sh.rescan_gen.fetch_add(1, Ordering::Relaxed);
            Response::Ok
        }
        Request::Quit => Response::Ok,
        Request::Handover => {
            sh.handover.store(true, Ordering::Relaxed);
            Response::Ok
        }
        Request::Keyboard { id, change } => {
            sh.keyboard_jobs.lock().unwrap().push((id.clone(), change.clone()));
            sh.wake();
            Response::Ok
        }
        Request::Mouse { id, change } => {
            sh.mouse_jobs.lock().unwrap().push((id.clone(), change.clone()));
            sh.wake();
            Response::Ok
        }
    }
}
