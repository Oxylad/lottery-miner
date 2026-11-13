//! Home server (oxyhome) over Windows OpenSSH (ssh.exe, BatchMode) and the
//! shared pool / mempool HTTP lookups.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::parse::{self, LogState};
use crate::settings::SshTarget;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Remote read-only probe. One ssh round trip; the 10 s energy gap is inside.
/// Only fixed commands; no user text is interpolated.
const PROBE: &str = "systemctl is-active cpuminer; echo ---SHOW; systemctl show cpuminer -p ActiveEnterTimestamp -p NRestarts -p LoadState; echo ---LOG; sudo -n journalctl -u cpuminer -o short-unix -n 300; echo ---TEMP; sensors | grep Package; echo ---ENERGY; E1=$(sudo -n cat /sys/class/powercap/intel-rapl:0/energy_uj); sleep 10; E2=$(sudo -n cat /sys/class/powercap/intel-rapl:0/energy_uj); echo $E1 $E2";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OxyhomeStats {
    pub service_active: bool,
    /// False when systemd reports LoadState=not-found for cpuminer.service.
    pub unit_found: bool,
    pub active_since: Option<String>,
    pub restarts: Option<u32>,
    pub log: LogState,
    pub temp_c: Option<f64>,
    /// Journal lines as (unix time, message), oldest first.
    pub journal: Vec<(f64, String)>,
    pub watts: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    fn word(self) -> &'static str {
        match self {
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
        }
    }
}

/// ssh.exe arguments for the target. The host may be `user@host` or an
/// ~/.ssh/config alias; port and key are only passed when set.
pub fn ssh_args(target: &SshTarget) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];
    if let Some(port) = target.port {
        args.push("-p".into());
        args.push(port.to_string());
    }
    let key = target.key_file.trim();
    if !key.is_empty() {
        args.push("-i".into());
        args.push(expand_home(key));
    }
    args.push(target.host.trim().to_string());
    args
}

/// `~/...` to the user's profile folder, as ssh.exe on Windows does not expand it for -i.
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return std::path::Path::new(&home).join(rest).display().to_string();
        }
    }
    path.to_string()
}

fn run_ssh(target: &SshTarget, remote: &str) -> Result<String, String> {
    let host = target.host.trim();
    if host.is_empty() {
        return Err("SSH host is not set".to_string());
    }
    if host.starts_with('-') || host.contains(char::is_whitespace) {
        return Err(format!("SSH host \"{host}\" is not valid"));
    }
    let mut args = ssh_args(target);
    args.push(remote.to_string());
    let out = Command::new("ssh.exe")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run ssh.exe: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "ssh failed ({}): {}{}",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            err.trim(),
            ssh_hint(&err, host)
        ))
    }
}

/// One-line advice for the common BatchMode failures.
fn ssh_hint(stderr: &str, host: &str) -> String {
    let e = stderr.to_ascii_lowercase();
    if e.contains("host key verification failed") {
        format!(" Run \"ssh {host}\" once in a terminal to check and accept the server's host key.")
    } else if e.contains("permission denied") {
        " The key was refused; set the SSH key field or load the key into ssh-agent.".into()
    } else if e.contains("could not resolve hostname") {
        " The host name does not resolve from this PC (is it on the same network or tailnet?).".into()
    } else if e.contains("timed out") {
        " No answer within 10 s; check the address, port and network.".into()
    } else if e.contains("sudo") && e.contains("password") {
        " sudo asked for a password; the server user needs passwordless sudo for these commands.".into()
    } else {
        String::new()
    }
}

pub fn probe(target: &SshTarget) -> Result<OxyhomeStats, String> {
    let text = run_ssh(target, PROBE)?;
    Ok(parse_probe(&text))
}

/// Splits the probe output on its ---MARKER lines and parses each part.
/// "1794600000.123456 host cpuminer[812]: message" -> (time, message).
/// Lines without a leading timestamp (e.g. "-- No entries --") give None.
fn split_journal_line(line: &str) -> Option<(f64, &str)> {
    let (ts, rest) = line.split_once(' ')?;
    let ts: f64 = ts.parse().ok()?;
    let (_, msg) = rest.split_once(": ")?;
    Some((ts, msg))
}

pub fn parse_probe(text: &str) -> OxyhomeStats {
    let mut stats = OxyhomeStats {
        unit_found: true,
        ..Default::default()
    };
    let mut section = "";
    let mut parts: std::collections::HashMap<&str, Vec<&str>> = Default::default();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("---") {
            section = name.trim();
            continue;
        }
        if section.is_empty() {
            // First line is systemctl is-active output.
            parts.entry("ACTIVE").or_default().push(line);
        } else {
            parts.entry(section).or_default().push(line);
        }
    }
    stats.service_active = parts
        .get("ACTIVE")
        .and_then(|v| v.first())
        .map(|s| s.trim() == "active")
        .unwrap_or(false);
    for line in parts.get("SHOW").into_iter().flatten() {
        if let Some(v) = line.strip_prefix("ActiveEnterTimestamp=") {
            stats.active_since = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("NRestarts=") {
            stats.restarts = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("LoadState=") {
            stats.unit_found = v.trim() != "not-found";
        }
    }
    let mut messages = Vec::new();
    for line in parts.get("LOG").into_iter().flatten() {
        match split_journal_line(line) {
            Some((ts, msg)) => {
                messages.push(msg.to_string());
                stats.journal.push((ts, msg.to_string()));
            }
            None => messages.push(line.to_string()),
        }
    }
    stats.log = parse::parse_journal(&messages.join("\n"));
    let temp_text = parts.get("TEMP").map(|v| v.join("\n")).unwrap_or_default();
    stats.temp_c = parse::parse_package_temp(&temp_text);
    if let Some(energy) = parts.get("ENERGY").and_then(|v| v.first()) {
        let nums: Vec<u64> = energy.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if nums.len() == 2 {
            stats.watts = parse::watts_from_energy(nums[0], nums[1], 10.0);
        }
    }
    stats
}

/// systemctl start|stop|restart cpuminer, and nothing else.
pub fn control(target: &SshTarget, action: ServiceAction) -> Result<(), String> {
    let remote = format!("sudo -n systemctl {} cpuminer", action.word());
    run_ssh(target, &remote).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_OUT: &str = "active
---SHOW
NRestarts=2
ActiveEnterTimestamp=Sun 2026-11-02 10:00:00 CET
---LOG
[2026-11-13 22:28:12] New Work: Block 975715, Tx 12, Netdiff 1.4836e+014, Job 6af4689e00001b80
                      Hash rate          0.00h/s     40.10Mh/s   (69.40Mh/s)
                      Submitted             3            9
                      Accepted              3            9      100.0%
---TEMP
Package id 0:  +61.0°C  (high = +86.0°C, crit = +100.0°C)
---ENERGY
1000000000 1820000000
";

    #[test]
    fn probe_output_is_parsed() {
        let s = parse_probe(PROBE_OUT);
        assert!(s.service_active);
        assert_eq!(s.restarts, Some(2));
        assert_eq!(s.active_since.as_deref(), Some("Sun 2026-11-02 10:00:00 CET"));
        assert_eq!(s.log.hashrate, Some(69.4e6));
        assert_eq!(s.log.accepted, 9);
        assert_eq!(s.temp_c, Some(61.0));
        assert_eq!(s.watts, Some(82.0));
    }

    #[test]
    fn short_unix_journal_lines() {
        // Real lines from oxyhome (journalctl -o short-unix).
        let out = "active\n---SHOW\nLoadState=loaded\n---LOG\n\
1794613114.204477 oxyhome cpuminer[1631584]: [2026-11-13 23:38:34] New Work: Block 975724, Tx 12, Netdiff 1.4836e+14, Job 6af4689e00001c8e\n\
1794613114.204817 oxyhome cpuminer[1631584]:                       TTF @ 69.39 Mh/s: Block 291183682y360d, Share 7d03h\n\
1794613144.235578 oxyhome cpuminer[1631584]:                       Hash rate          0.00h/s     40.10Mh/s   (69.40Mh/s)\n\
---TEMP\n---ENERGY\n";
        let s = parse_probe(out);
        assert_eq!(s.journal.len(), 3);
        assert_eq!(s.journal[0].0, 1794613114.204477);
        assert!(s.journal[0].1.starts_with("[2026-11-13 23:38:34] New Work"));
        // The message keeps its leading spaces, so summary lines still parse.
        assert_eq!(s.log.block, Some(975724));
        assert_eq!(s.log.hashrate, Some(69.4e6));
        assert_eq!(split_journal_line("-- No entries --"), None);
    }

    #[test]
    fn missing_unit_is_detected() {
        // Real output from a server without cpuminer.service.
        let s = parse_probe("inactive\n---SHOW\nNRestarts=0\nLoadState=not-found\nActiveEnterTimestamp=\n---LOG\n---TEMP\n---ENERGY\n");
        assert!(!s.unit_found);
        assert!(!s.service_active);
        assert!(parse_probe(PROBE_OUT).unit_found);
    }

    #[test]
    fn inactive_service_and_empty_sections() {
        let s = parse_probe("inactive\n---SHOW\n---LOG\n---TEMP\n---ENERGY\n\n");
        assert!(!s.service_active);
        assert_eq!(s.log.hashrate, None);
        assert_eq!(s.watts, None);
    }

    #[test]
    fn ssh_args_forms() {
        let mut t = SshTarget::default();
        t.host = "oxyhome".into();
        assert_eq!(
            ssh_args(&t),
            vec!["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "oxyhome"]
        );
        t.host = "oxylad@oxyhome.tail4cf410.ts.net".into();
        t.port = Some(2222);
        t.key_file = r"C:\keys\id".into();
        let a = ssh_args(&t);
        assert_eq!(&a[4..], &["-p", "2222", "-i", r"C:\keys\id", "oxylad@oxyhome.tail4cf410.ts.net"]);
    }

    #[test]
    fn bad_host_is_rejected_without_running_ssh() {
        let mut t = SshTarget::default();
        t.host = "-oProxyCommand=calc".into();
        assert!(run_ssh(&t, "true").is_err());
        t.host = "".into();
        assert!(run_ssh(&t, "true").is_err());
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .user_agent("lottery-miner")
        .build()
}

/// GET a JSON document; 404 gives Ok(None) (normal for a pool user with no shares yet).
pub fn get_json(url: &str) -> Result<Option<serde_json::Value>, String> {
    match agent().get(url).call() {
        Ok(resp) => resp
            .into_json::<serde_json::Value>()
            .map(Some)
            .map_err(|e| format!("{url}: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(format!("{url}: {e}")),
    }
}
