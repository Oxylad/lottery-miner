//! Small Win32 helpers: show/hide the GPUI window by HWND, the HKCU Run key,
//! and opening a file in Notepad. No extra crates.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[link(name = "user32")]
unsafe extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
}

pub fn hide_window(hwnd: isize) {
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn show_window(hwnd: isize) {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd);
    }
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "LotteryMiner";

/// Adds or removes the HKCU Run entry that starts the app hidden at login.
pub fn set_start_at_login(enabled: bool) -> Result<(), String> {
    let mut cmd = Command::new("reg.exe");
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let data = format!("\"{}\" --hidden", exe.display());
        cmd.args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &data, "/f"]);
    } else {
        cmd.args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("reg.exe: {e}"))?;
    // Deleting a value that does not exist is fine.
    if out.status.success() || !enabled {
        Ok(())
    } else {
        Err(format!(
            "reg.exe: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn open_in_notepad(path: &std::path::Path) -> Result<(), String> {
    Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("notepad: {e}"))
}
