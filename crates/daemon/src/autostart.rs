//! Start at logon through Task Scheduler: the daemon needs admin rights for PawnIO, and a scheduled
//! task with "highest privileges" starts elevated without a UAC prompt.
use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK: &str = "Cool'n RGB";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks(args: &[&str]) -> Result<(), String> {
    let out = Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn set(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let tr = format!("\"{}\"", exe.display());
        schtasks(&["/Create", "/F", "/TN", TASK, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST", "/DELAY", "0000:10"])
    } else {
        schtasks(&["/Delete", "/F", "/TN", TASK]).or(Ok(()))
    }
}
