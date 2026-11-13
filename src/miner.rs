//! Local cpuminer-opt instances: start at idle priority with no console,
//! read their stdout into a shared LogState, query their stats API, and
//! kill only the processes this app started (by Child handle).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::parse::{self, ApiSummary, LogState};
use crate::logbook::{SharedLog, Source};
use crate::settings::Settings;

const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Instance {
    pub index: u32,
    pub port: u16,
    pub started: Instant,
    pub log: Arc<Mutex<LogState>>,
    child: Child,
}

/// What the UI needs from one instance without holding the child.
#[derive(Clone)]
pub struct InstanceHandle {
    pub index: u32,
    pub port: u16,
    pub log: Arc<Mutex<LogState>>,
    pub started: Instant,
}

#[derive(Default)]
pub struct Miner {
    instances: Vec<Instance>,
    /// Where every output line also goes (the Logs tab).
    book: Option<SharedLog>,
}

pub fn command_line(settings: &Settings, index: u32, port: u16) -> Vec<String> {
    let user = format!("{}.{}", settings.address.trim(), settings.worker.trim());
    vec![
        "-a".into(),
        "sha256d".into(),
        "-o".into(),
        settings.pool_url.trim().into(),
        "-u".into(),
        user,
        "-p".into(),
        settings.password.clone(),
        "-t".into(),
        settings.threads_per_instance.to_string(),
        "--cpu-affinity".into(),
        format!("0x{:04X}", settings.affinity_mask(index)),
        "--no-color".into(),
        "-b".into(),
        format!("127.0.0.1:{port}"),
    ]
}

/// gpu-miner.exe: the configured path, or next to this app's exe.
pub fn gpu_miner_path(settings: &Settings) -> std::path::PathBuf {
    let configured = settings.gpu.exe.trim();
    if !configured.is_empty() {
        return configured.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("gpu-miner.exe")))
        .unwrap_or_else(|| "gpu-miner.exe".into())
}

pub fn gpu_command_line(settings: &Settings) -> Vec<String> {
    let g = &settings.gpu;
    let mut args = vec![
        "-o".into(),
        settings.pool_url.trim().into(),
        "-u".into(),
        format!("{}.{}", settings.address.trim(), g.worker.trim()),
        "-p".into(),
        settings.password.clone(),
        "-b".into(),
        format!("127.0.0.1:{}", g.api_port),
        "--intensity".into(),
        g.intensity.to_string(),
    ];
    if !g.device.trim().is_empty() {
        args.push("--device".into());
        args.push(g.device.trim().into());
    }
    args
}

impl Miner {
    pub fn with_log(book: SharedLog) -> Self {
        Self {
            instances: Vec::new(),
            book: Some(book),
        }
    }

    pub fn is_running(&self) -> bool {
        !self.instances.is_empty()
    }

    #[cfg(test)]
    pub fn pids(&self) -> Vec<u32> {
        self.instances.iter().map(|i| i.child.id()).collect()
    }

    pub fn handles(&self) -> Vec<InstanceHandle> {
        self.instances
            .iter()
            .map(|i| InstanceHandle {
                index: i.index,
                port: i.port,
                log: i.log.clone(),
                started: i.started,
            })
            .collect()
    }

    /// Starts all instances from settings. Returns error text on the first failure;
    /// instances already started stay running so the caller can stop them.
    pub fn start(&mut self, settings: &Settings) -> Result<(), String> {
        if self.is_running() {
            return Ok(());
        }
        let problems = settings.validate();
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        for i in 0..settings.instances {
            let port = settings.api_base_port + i as u16;
            let args = command_line(settings, i, port);
            self.spawn_instance(&settings.miner_exe, &args, i, port, Source::Cpu(i))?;
        }
        Ok(())
    }

    /// Starts the GPU miner (one process) from settings.
    pub fn start_gpu(&mut self, settings: &Settings) -> Result<(), String> {
        if self.is_running() {
            return Ok(());
        }
        let exe = gpu_miner_path(settings);
        if !exe.exists() {
            return Err(format!("GPU miner not found at {}", exe.display()));
        }
        let args = gpu_command_line(settings);
        self.spawn_instance(&exe.display().to_string(), &args, 0, settings.gpu.api_port, Source::Gpu)
    }

    fn spawn_instance(
        &mut self,
        exe: &str,
        args: &[String],
        index: u32,
        port: u16,
        source: Source,
    ) -> Result<(), String> {
        if let Some(b) = &self.book {
            if let Ok(mut b) = b.lock() {
                b.push(Source::App, &format!("starting {}: {} {}", source.label(), exe, args.join(" ")));
            }
        }
        let mut child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(IDLE_PRIORITY_CLASS | CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("cannot start {exe}: {e}"))?;
        let log = Arc::new(Mutex::new(LogState::default()));
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, log.clone(), self.book.clone(), source);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, log.clone(), self.book.clone(), source);
        }
        self.instances.push(Instance {
            index,
            port,
            started: Instant::now(),
            log,
            child,
        });
        Ok(())
    }

    /// Kills only the children this app started, then waits for them.
    pub fn stop(&mut self) {
        if !self.instances.is_empty() {
            if let Some(Ok(mut b)) = self.book.as_ref().map(|b| b.lock()) {
                let names: Vec<String> = self.instances.iter().map(|i| format!("pid {}", i.child.id())).collect();
                b.push(Source::App, &format!("stopping {}", names.join(", ")));
            }
        }
        for inst in self.instances.iter_mut() {
            let _ = inst.child.kill();
            let _ = inst.child.wait();
        }
        self.instances.clear();
    }

    /// Leaves the children running and forgets them (Quit with "stop miners
    /// on Quit" off). They then show up as a foreign miner on the next launch.
    pub fn detach(&mut self) {
        self.instances.clear();
    }

    /// Like reap_exited, but returns why each exited instance stopped
    /// (its last error line, or its exit code).
    pub fn reap_exited_errors(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        self.instances.retain_mut(|i| match i.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                let last = i.log.lock().ok().and_then(|l| l.last_error.clone());
                out.push(last.unwrap_or_else(|| format!("exit code {:?}", status.code())));
                false
            }
            Err(e) => {
                out.push(e.to_string());
                false
            }
        });
        out
    }

    /// Drops instances whose process has exited on its own.
    pub fn reap_exited(&mut self) -> usize {
        let before = self.instances.len();
        self.instances.retain_mut(|i| matches!(i.child.try_wait(), Ok(None)));
        before - self.instances.len()
    }
}

/// A panic or an unexpected exit must not leave our miners orphaned.
impl Drop for Miner {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    log: Arc<Mutex<LogState>>,
    book: Option<SharedLog>,
    source: Source,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(mut st) = log.lock() {
                st.feed(&line);
            }
            if let Some(b) = &book {
                if let Ok(mut b) = b.lock() {
                    b.push(source, &line);
                }
            }
        }
    });
}

/// Sends `summary` to a cpuminer API port and parses the reply.
pub fn query_summary(port: u16) -> Result<ApiSummary, String> {
    let reply = api_request(port, "summary")?;
    parse::parse_api_summary(&reply).ok_or_else(|| "unexpected API reply".to_string())
}

pub fn api_request(port: u16, command: &str) -> Result<String, String> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().map_err(|_| "bad port")?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800))
        .map_err(|e| format!("API port {port}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream
        .write_all(command.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'|') {
                    break;
                }
            }
            Err(e) => {
                if buf.is_empty() {
                    return Err(format!("API port {port}: {e}"));
                }
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetPriorityClass(h: isize) -> u32;
        fn GetExitCodeProcess(h: isize, code: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    fn priority(pid: u32) -> Option<u32> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return None;
            }
            let p = GetPriorityClass(h);
            CloseHandle(h);
            Some(p)
        }
    }

    fn alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return false;
            }
            let mut code = 0u32;
            GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            code == STILL_ACTIVE
        }
    }

    /// Live test against the real miner: `cargo test -- --ignored live_`.
    /// Starts the configured instances, checks idle priority and the API,
    /// stops them and checks exactly those PIDs are gone.
    #[test]
    #[ignore]
    fn live_start_query_stop() {
        let s = Settings::default();
        let mut m = Miner::default();
        m.start(&s).expect("start");
        let pids = m.pids();
        assert_eq!(pids.len(), s.instances as usize);
        for pid in &pids {
            assert_eq!(priority(*pid), Some(IDLE_PRIORITY_CLASS), "pid {pid} not idle priority");
        }
        // Wait until every instance logged "Stratum connection established",
        // then give the hash meters 30 s to settle.
        let t0 = Instant::now();
        while !m.handles().iter().all(|h| h.log.lock().unwrap().connected) {
            assert!(t0.elapsed() < Duration::from_secs(120), "not all instances connected");
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("all connected after {:?}", t0.elapsed());
        std::thread::sleep(Duration::from_secs(30));
        let mut total = 0.0;
        for h in m.handles() {
            let sum = query_summary(h.port).expect("API answers");
            let log = h.log.lock().unwrap().clone();
            println!(
                "instance {} port {}: {:.2} MH/s, stalls {}, connected {}",
                h.index,
                h.port,
                sum.hashrate / 1e6,
                log.stall_count,
                log.connected
            );
            assert!(log.connected, "stdout log was captured");
            total += sum.hashrate;
        }
        println!("total {:.2} MH/s", total / 1e6);
        m.stop();
        std::thread::sleep(Duration::from_millis(500));
        for pid in &pids {
            assert!(!alive(*pid), "pid {pid} still running");
        }
    }

    /// Live test of the app's GPU path: `cargo test -- --ignored live_gpu`.
    /// Needs target\release\gpu-miner.exe (cargo build --release --workspace).
    #[test]
    #[ignore]
    fn live_gpu_start_query_stop() {
        let mut s = Settings::default();
        s.gpu.exe = concat!(env!("CARGO_MANIFEST_DIR"), r"\target\release\gpu-miner.exe").into();
        s.gpu.api_port = 4070; // away from a running app's port
        let mut m = Miner::default();
        m.start_gpu(&s).expect("start gpu");
        let pids = m.pids();
        assert_eq!(pids.len(), 1);
        assert_eq!(priority(pids[0]), Some(IDLE_PRIORITY_CLASS));
        let t0 = Instant::now();
        while !m.handles()[0].log.lock().unwrap().connected {
            assert!(t0.elapsed() < Duration::from_secs(60), "gpu-miner did not connect");
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_secs(20));
        let sum = query_summary(4070).expect("gpu API answers");
        println!(
            "gpu: {:.2} GH/s on {:?}, connected after {:?}",
            sum.hashrate / 1e9,
            sum.device,
            t0.elapsed()
        );
        assert!(sum.hashrate > 1e8, "GPU hashrate too low: {}", sum.hashrate);
        m.stop();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!alive(pids[0]), "gpu-miner still running");
    }

    #[test]
    fn gpu_command_line_from_settings() {
        let mut s = Settings::default();
        let joined = gpu_command_line(&s).join(" ");
        assert!(joined.contains("-o stratum+tcp://eusolo.ckpool.org:3333"));
        assert!(joined.contains("-u bc1qq4ndwhp5r350h9g7pygdgcu52hrg8744v3xmgk.desktopgpu"));
        assert!(joined.contains("-b 127.0.0.1:4060"));
        assert!(joined.contains("--intensity 25"));
        assert!(!joined.contains("--device"));
        s.gpu.device = "7700".into();
        assert!(gpu_command_line(&s).join(" ").contains("--device 7700"));
    }

    #[test]
    fn command_line_matches_brief() {
        let s = Settings::default();
        let args = command_line(&s, 1, 4049);
        let joined = args.join(" ");
        assert!(joined.contains("-a sha256d"));
        assert!(joined.contains("-o stratum+tcp://eusolo.ckpool.org:3333"));
        assert!(joined.contains(
            "-u bc1qq4ndwhp5r350h9g7pygdgcu52hrg8744v3xmgk.desktop"
        ));
        assert!(joined.contains("-t 4"));
        assert!(joined.contains("--cpu-affinity 0x00F0"));
        assert!(joined.contains("--no-color"));
        assert!(joined.contains("-b 127.0.0.1:4049"));
    }
}
