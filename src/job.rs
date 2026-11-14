//! A Windows Job Object that owns every miner this app starts. With
//! JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE set, Windows kills the miners when the
//! app's process ends for any reason (Quit, End task, crash), so they can
//! never be left running without the app. Quit with "stop miners on Quit"
//! off clears the flag first, so the miners deliberately keep running.

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::sync::OnceLock;

const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

#[repr(C)]
#[derive(Default)]
struct BasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Default)]
struct ExtendedLimitInformation {
    basic: BasicLimitInformation,
    io: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const core::ffi::c_void, name: *const u16) -> isize;
    fn SetInformationJobObject(job: isize, class: i32, info: *const core::ffi::c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
    #[cfg(test)]
    fn IsProcessInJob(process: isize, job: isize, result: *mut i32) -> i32;
    fn GetLastError() -> u32;
}

/// The job handle lives for the whole process; Windows closes it at exit.
static JOB: OnceLock<Option<isize>> = OnceLock::new();

fn set_kill_flag(job: isize, kill: bool) -> bool {
    let mut info = ExtendedLimitInformation::default();
    if kill {
        info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    unsafe {
        SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<ExtendedLimitInformation>() as u32,
        ) != 0
    }
}

fn job() -> Option<isize> {
    *JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if h == 0 || !set_kill_flag(h, true) {
            None
        } else {
            Some(h)
        }
    })
}

/// Puts a freshly started miner into the job.
pub fn adopt(child: &Child) -> Result<(), String> {
    let job = job().ok_or("could not create the miners' job object")?;
    let process = child.as_raw_handle() as isize;
    if unsafe { AssignProcessToJobObject(job, process) } == 0 {
        return Err(format!("AssignProcessToJobObject failed (error {})", unsafe { GetLastError() }));
    }
    Ok(())
}

#[cfg(test)]
/// Whether the child is in the miners' job (for tests).
pub fn owns(child: &Child) -> bool {
    let Some(job) = job() else { return false };
    let mut result = 0i32;
    unsafe { IsProcessInJob(child.as_raw_handle() as isize, job, &mut result) != 0 && result != 0 }
}

/// Called on Quit: false lets the miners outlive the app on purpose.
pub fn kill_miners_on_exit(kill: bool) {
    if let Some(job) = job() {
        set_kill_flag(job, kill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn struct_size_matches_windows() {
        // sizeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION) on x64 Windows.
        assert_eq!(std::mem::size_of::<ExtendedLimitInformation>(), 144);
    }

    #[test]
    fn child_is_adopted() {
        let mut child = Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn helper");
        adopt(&child).expect("adopt");
        assert!(owns(&child));
        let _ = child.kill();
        let _ = child.wait();
    }
}
