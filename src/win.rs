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

#[repr(C)]
struct LastInputInfo {
    cb_size: u32,
    dw_time: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetLastInputInfo(plii: *mut LastInputInfo) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
    fn SetThreadExecutionState(flags: u32) -> u32;
}

const ES_CONTINUOUS: u32 = 0x8000_0000;
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

/// Seconds since the last keyboard or mouse input in this session.
pub fn idle_seconds() -> u64 {
    let mut info = LastInputInfo {
        cb_size: std::mem::size_of::<LastInputInfo>() as u32,
        dw_time: 0,
    };
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        // Both are 32-bit millisecond tick counts; wrapping_sub handles rollover.
        (GetTickCount().wrapping_sub(info.dw_time) / 1000) as u64
    }
}

/// Keeps the PC from sleeping (the display may still turn off) while `on`.
/// Applies to the calling thread, so call it from the GPUI main thread.
pub fn keep_awake(on: bool) {
    unsafe {
        SetThreadExecutionState(if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        });
    }
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
}

/// Size Windows uses for small icons like the tray (16 at 100% scaling).
pub fn small_icon_size() -> u32 {
    const SM_CXSMICON: i32 = 49;
    let s = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if s > 0 { s as u32 } else { 16 }
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

#[link(name = "shell32")]
unsafe extern "system" {
    fn IsUserAnAdmin() -> i32;
}

/// True when this process runs with administrator rights (elevated).
pub fn is_elevated() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}
