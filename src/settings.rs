//! Settings stored as JSON in %APPDATA%\lottery-miner\settings.json.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Above this speed per instance cpuminer-opt's ckpool stall gets worse, see notes.
pub const MAX_MHS_PER_INSTANCE: f64 = 140.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshTarget {
    /// Display name for the machine.
    pub name: String,
    /// `user@hostname`, a bare hostname, or a Host alias from ~/.ssh/config.
    pub host: String,
    /// None: the default (22) or whatever ~/.ssh/config says.
    pub port: Option<u16>,
    /// Private key path. Empty: use ~/.ssh/config or the agent.
    pub key_file: String,
}

impl Default for SshTarget {
    fn default() -> Self {
        Self {
            name: "oxyhome".to_string(),
            host: "oxylad@oxyhome".to_string(),
            port: None,
            key_file: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSettings {
    /// Path to gpu-miner.exe; empty = next to this app.
    pub exe: String,
    /// Pool worker name for the GPU (user = <address>.<worker>).
    pub worker: String,
    pub api_port: u16,
    /// 2^intensity nonces per GPU dispatch (16-30). Lower = smoother desktop.
    pub intensity: u32,
    /// GPU name filter; empty = first discrete GPU.
    pub device: String,
    pub start_on_launch: bool,
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            exe: String::new(),
            worker: "desktopgpu".to_string(),
            api_port: 4060,
            intensity: 25,
            device: String::new(),
            start_on_launch: false,
        }
    }
}

/// Choices for the idle slider, in minutes.
pub const IDLE_STEPS: [u32; 12] = [1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 90, 120];

/// Start mining after the PC has had no keyboard/mouse input for a while,
/// stop again on the next input (only what idle mode itself started).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdleSettings {
    pub enabled: bool,
    pub minutes: u32,
    pub cpu: bool,
    pub gpu: bool,
    /// Prevent sleep while idle mining is running.
    pub keep_awake: bool,
}

impl Default for IdleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            minutes: 10,
            cpu: true,
            gpu: true,
            keep_awake: true,
        }
    }
}

/// What idle mode should do now, given the idle time. Pure, for tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IdleAction {
    /// Idle long enough: start the selected miners (if not running).
    Start,
    /// Input seen: stop what idle mode started.
    Stop,
    Nothing,
}

pub fn idle_action(s: &IdleSettings, idle_secs: u64, idle_running: bool) -> IdleAction {
    let threshold = s.minutes.max(1) as u64 * 60;
    if !s.enabled {
        return if idle_running { IdleAction::Stop } else { IdleAction::Nothing };
    }
    if idle_secs >= threshold {
        IdleAction::Start
    } else if idle_running {
        IdleAction::Stop
    } else {
        IdleAction::Nothing
    }
}

/// Parses the port field of the form: empty means default.
pub fn parse_port(text: &str) -> Result<Option<u16>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("SSH port \"{t}\" is not a number from 1 to 65535")),
        Ok(p) => Ok(Some(p)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub miner_exe: String,
    pub address: String,
    pub pool_url: String,
    pub worker: String,
    pub password: String,
    pub instances: u32,
    pub threads_per_instance: u32,
    pub api_base_port: u16,
    pub start_mining_on_launch: bool,
    pub start_at_login: bool,
    pub stop_miners_on_quit: bool,
    pub oxyhome: SshTarget,
    pub refresh_secs: u64,
    pub gpu: GpuSettings,
    pub idle: IdleSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            miner_exe: r"C:\cpuminer\cpuminer-avx512-sha-vaes.exe".to_string(),
            address: "bc1qq4ndwhp5r350h9g7pygdgcu52hrg8744v3xmgk".to_string(),
            pool_url: "stratum+tcp://eusolo.ckpool.org:3333".to_string(),
            worker: "desktop".to_string(),
            password: "x".to_string(),
            instances: 4,
            threads_per_instance: 4,
            api_base_port: 4048,
            start_mining_on_launch: false,
            start_at_login: false,
            stop_miners_on_quit: true,
            oxyhome: SshTarget::default(),
            refresh_secs: 5,
            gpu: GpuSettings::default(),
            idle: IdleSettings::default(),
        }
    }
}

impl Settings {
    /// CPU affinity mask for instance `i`, e.g. 0x000F, 0x00F0, 0x0F00, 0xF000 for 4 threads.
    pub fn affinity_mask(&self, i: u32) -> u64 {
        let t = self.threads_per_instance.clamp(1, 16);
        let block = (1u64 << t) - 1;
        block << (t * i)
    }

    /// Problems with the current values, empty when all is fine.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.threads_per_instance == 0 || self.threads_per_instance > 16 {
            out.push("threads per instance must be 1 to 16".to_string());
        }
        if self.instances == 0 || self.instances > 8 {
            out.push("instances must be 1 to 8".to_string());
        }
        if self.instances * self.threads_per_instance > 16 {
            out.push("instances x threads exceeds 16 logical CPUs".to_string());
        }
        if self.address.trim().is_empty() {
            out.push("bitcoin address is empty".to_string());
        }
        if self.refresh_secs < 2 {
            out.push("refresh interval must be at least 2 seconds".to_string());
        }
        if !(16..=30).contains(&self.gpu.intensity) {
            out.push("GPU intensity must be 16 to 30".to_string());
        }
        if self.gpu.worker.trim().is_empty() {
            out.push("GPU worker name is empty".to_string());
        }
        out
    }
}

/// %APPDATA%\lottery-miner\settings.json, falling back to the working directory.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lottery-miner").join("settings.json")
}

/// Loads settings; a missing file gives the defaults, a broken file is an error.
pub fn load_from(path: &Path) -> Result<Settings, String> {
    match fs::read_to_string(path) {
        // Editors on Windows may add a UTF-8 BOM, which serde_json rejects.
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("settings: {e}")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("settings: {e}")),
    }
}

pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("settings dir: {e}"))?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| format!("settings write: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lm-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn round_trip() {
        let path = temp_file("roundtrip");
        let mut s = Settings::default();
        s.instances = 2;
        s.oxyhome.host = "oxylad@oxyhome.tail4cf410.ts.net".into();
        s.oxyhome.port = Some(2222);
        s.oxyhome.key_file = r"C:\Users\me\.ssh\id_ed25519".into();
        save_to(&path, &s).unwrap();
        let back = load_from(&path).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn missing_file_gives_defaults_and_partial_file_fills_defaults() {
        let path = temp_file("missing");
        assert_eq!(load_from(&path).unwrap(), Settings::default());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"instances": 3}"#).unwrap();
        let s = load_from(&path).unwrap();
        assert_eq!(s.instances, 3);
        assert_eq!(s.threads_per_instance, 4);
        // Files written before GPU support get the GPU defaults.
        assert_eq!(s.gpu, GpuSettings::default());
    }

    #[test]
    fn bom_is_accepted() {
        let path = temp_file("bom");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "\u{feff}{\"worker\": \"pc\"}").unwrap();
        assert_eq!(load_from(&path).unwrap().worker, "pc");
    }

    #[test]
    fn broken_file_is_error() {
        let path = temp_file("broken");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_err());
    }

    #[test]
    fn affinity_masks_match_brief() {
        let s = Settings::default();
        assert_eq!(s.affinity_mask(0), 0x000F);
        assert_eq!(s.affinity_mask(1), 0x00F0);
        assert_eq!(s.affinity_mask(2), 0x0F00);
        assert_eq!(s.affinity_mask(3), 0xF000);
    }

    #[test]
    fn idle_decisions() {
        let mut s = IdleSettings {
            enabled: true,
            minutes: 10,
            ..Default::default()
        };
        assert_eq!(idle_action(&s, 0, false), IdleAction::Nothing);
        assert_eq!(idle_action(&s, 599, false), IdleAction::Nothing);
        assert_eq!(idle_action(&s, 600, false), IdleAction::Start);
        assert_eq!(idle_action(&s, 3600, true), IdleAction::Start);
        // Mouse moved: idle time drops below the threshold.
        assert_eq!(idle_action(&s, 0, true), IdleAction::Stop);
        // Turning the feature off stops what it started.
        s.enabled = false;
        assert_eq!(idle_action(&s, 3600, true), IdleAction::Stop);
        assert_eq!(idle_action(&s, 3600, false), IdleAction::Nothing);
        // 0 minutes is treated as 1.
        s.enabled = true;
        s.minutes = 0;
        assert_eq!(idle_action(&s, 59, false), IdleAction::Nothing);
        assert_eq!(idle_action(&s, 60, false), IdleAction::Start);
    }

    #[test]
    fn port_field() {
        assert_eq!(parse_port(""), Ok(None));
        assert_eq!(parse_port(" 22 "), Ok(Some(22)));
        assert!(parse_port("0").is_err());
        assert!(parse_port("abc").is_err());
        assert!(parse_port("70000").is_err());
    }

    #[test]
    fn validation_catches_oversubscription() {
        let mut s = Settings::default();
        assert!(s.validate().is_empty());
        s.threads_per_instance = 16;
        assert!(!s.validate().is_empty());
    }
}
