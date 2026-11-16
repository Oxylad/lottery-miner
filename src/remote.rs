//! Home server (oxyhome) over Windows OpenSSH (ssh.exe, BatchMode) and the
//! shared pool / mempool HTTP lookups.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::parse::{self, LogState};
use crate::settings::{Coin, MoneroSettings, SshTarget, is_monero_address, is_pool_host_port};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Remote read-only probe. One ssh round trip; the 10 s energy gap is inside.
/// Only fixed commands; no user text is interpolated.
/// Reports cpuminer and xmrig, then details for the selected unit: xmrig if it is
/// enabled or running (the app switched the server to Monero), else cpuminer.
const PROBE: &str = "systemctl is-active cpuminer; echo ---XMRIG; systemctl is-active xmrig; U=cpuminer; systemctl is-enabled --quiet xmrig && U=xmrig; systemctl is-active --quiet xmrig && U=xmrig; echo ---UNIT; echo $U; echo ---SHOW; systemctl show $U -p ActiveEnterTimestamp -p NRestarts -p LoadState; echo ---LOG; sudo -n journalctl -u $U -o short-unix -n 300; echo ---NODE; curl -s -m 5 http://127.0.0.1:18081/get_info 2>/dev/null; echo; echo ---TEMP; sensors | grep Package; echo ---ENERGY; E1=$(sudo -n cat /sys/class/powercap/intel-rapl:0/energy_uj); sleep 10; E2=$(sudo -n cat /sys/class/powercap/intel-rapl:0/energy_uj); echo $E1 $E2";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OxyhomeStats {
    pub service_active: bool,
    /// The miner unit the server is set to: "cpuminer" (Bitcoin) or "xmrig" (Monero).
    pub unit: String,
    /// False when systemd reports LoadState=not-found for cpuminer.service.
    pub unit_found: bool,
    pub active_since: Option<String>,
    pub restarts: Option<u32>,
    pub log: LogState,
    pub temp_c: Option<f64>,
    /// Monero node (monerod) status, if one runs on the server.
    pub node: Option<NodeStatus>,
    /// Journal lines as (unix time, message), oldest first.
    pub journal: Vec<(f64, String)>,
    pub watts: Option<f64>,
}

/// From monerod's get_info (server-local full RPC).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeStatus {
    pub height: u64,
    /// Network height the node is syncing towards (0 or below height once synced).
    pub target_height: u64,
    pub synchronized: bool,
    pub out_peers: u64,
    pub in_peers: u64,
    pub database_bytes: u64,
    pub version: String,
}

impl NodeStatus {
    /// Sync progress 0..=1.
    pub fn progress(&self) -> f64 {
        if self.synchronized {
            return 1.0;
        }
        let target = self.target_height.max(self.height);
        if target == 0 { 0.0 } else { self.height as f64 / target as f64 }
    }

    pub fn remaining_blocks(&self) -> u64 {
        if self.synchronized { 0 } else { self.target_height.saturating_sub(self.height) }
    }
}

/// Parses monerod's get_info JSON; None if empty or not a node reply.
pub fn parse_node_info(json: &str) -> Option<NodeStatus> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    if v.get("status")?.as_str()? != "OK" {
        return None;
    }
    let u = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Some(NodeStatus {
        height: u("height"),
        target_height: u("target_height"),
        synchronized: v.get("synchronized").and_then(|x| x.as_bool()).unwrap_or(false),
        out_peers: u("outgoing_connections_count"),
        in_peers: u("incoming_connections_count"),
        database_bytes: u("database_size"),
        version: v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
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
    let first_is_active = |key: &str| {
        parts
            .get(key)
            .and_then(|v| v.first())
            .map(|s| s.trim() == "active")
            .unwrap_or(false)
    };
    stats.unit = parts
        .get("UNIT")
        .and_then(|v| v.first())
        .map(|s| s.trim())
        .filter(|s| *s == "xmrig")
        .unwrap_or("cpuminer")
        .to_string();
    stats.service_active = if stats.unit == "xmrig" {
        first_is_active("XMRIG")
    } else {
        first_is_active("ACTIVE")
    };
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
    stats.node = parts.get("NODE").and_then(|v| parse_node_info(&v.join("\n")));
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
/// systemctl start|stop|restart of the miner unit for `coin`, and nothing else.
pub fn control(target: &SshTarget, action: ServiceAction, coin: Coin) -> Result<(), String> {
    let remote = format!("sudo -n systemctl {} {}", action.word(), coin.server_unit());
    run_ssh(target, &remote).map(|_| ())
}

/// The remote commands that switch the server's miner to `coin`. For Monero
/// it first writes /etc/default/xmrig (pool, address, rig name); both values
/// are validated so nothing but Base58 and host:port reaches the shell.
pub fn switch_script(coin: Coin, monero: &MoneroSettings) -> Result<String, String> {
    match coin {
        Coin::Bitcoin => Ok(
            "sudo -n systemctl disable --now xmrig && sudo -n systemctl enable --now cpuminer".into(),
        ),
        Coin::Monero => {
            let addr = monero.address.trim();
            let pool = monero.pool.trim();
            if !is_monero_address(addr) {
                return Err("Set a valid Monero address first.".into());
            }
            if let Some(hint) = crate::settings::monero_node_port_hint(pool) {
                return Err(hint.to_string());
            }
            if !is_pool_host_port(pool) {
                return Err(format!("Monero pool \"{pool}\" must be host:port"));
            }
            Ok(format!(
                "printf 'XMRIG_POOL=%s\\nXMRIG_USER=%s\\nXMRIG_RIG=oxyhome\\n' {pool} {addr} | sudo -n tee /etc/default/xmrig >/dev/null && sudo -n systemctl disable --now cpuminer && sudo -n systemctl enable --now xmrig"
            ))
        }
    }
}

/// Switches the home server between cpuminer (Bitcoin) and xmrig (Monero),
/// including which one starts at boot.
pub fn switch_coin(target: &SshTarget, coin: Coin, monero: &MoneroSettings) -> Result<(), String> {
    let script = switch_script(coin, monero)?;
    run_ssh(target, &script).map(|_| ())
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
    fn probe_reports_xmrig_when_selected() {
        let out = "inactive\n---XMRIG\nactive\n---UNIT\nxmrig\n---SHOW\nLoadState=loaded\nNRestarts=0\n---LOG\n\
1794844000.100000 oxyhome xmrig[2201]: [2026-11-16 17:01:00.000]  miner    speed 10s/60s/15m 3456.7 3400.1 n/a H/s max 3500.0 H/s\n\
1794844060.100000 oxyhome xmrig[2201]: [2026-11-16 17:02:00.000]  cpu      accepted (2/0) diff 50000 (60 ms)\n\
---TEMP\n---ENERGY\n";
        let s = parse_probe(out);
        assert_eq!(s.unit, "xmrig");
        assert!(s.service_active, "uses xmrig's state, not cpuminer's");
        assert_eq!(s.log.hashrate, Some(3456.7));
        assert_eq!(s.log.accepted, 2);
        // Old-style output (no UNIT section) means cpuminer.
        assert_eq!(parse_probe(PROBE_OUT).unit, "cpuminer");
        assert!(parse_probe(PROBE_OUT).service_active);
    }

    #[test]
    fn node_status_from_probe() {
        // get_info captured from oxyhome during the first sync (trimmed).
        let info = r#"{
  "busy_syncing": false,
  "database_size": 4310794240,
  "height": 389872,
  "incoming_connections_count": 0,
  "outgoing_connections_count": 12,
  "status": "OK",
  "synchronized": false,
  "target_height": 3807787,
  "version": "0.18.5.4-release"
}"#;
        let out = format!("active\n---SHOW\nLoadState=loaded\n---LOG\n---NODE\n{info}\n\n---TEMP\n---ENERGY\n");
        let s = parse_probe(&out);
        let n = s.node.expect("node parsed");
        assert_eq!(n.height, 389872);
        assert_eq!(n.out_peers, 12);
        assert!(!n.synchronized);
        assert!((n.progress() - 0.1024).abs() < 0.001, "got {}", n.progress());
        assert_eq!(n.remaining_blocks(), 3807787 - 389872);
        assert_eq!(n.version, "0.18.5.4-release");
        // No node (curl printed nothing) and a synced node.
        assert!(parse_probe("active\n---NODE\n\n---TEMP\n").node.is_none());
        let synced = parse_node_info(r#"{"status":"OK","height":3807790,"target_height":0,"synchronized":true}"#).unwrap();
        assert_eq!(synced.progress(), 1.0);
        assert_eq!(synced.remaining_blocks(), 0);
    }

    #[test]
    fn switch_scripts_only_touch_the_two_miner_units() {
        let mut m = MoneroSettings::default();
        let btc = switch_script(Coin::Bitcoin, &m).unwrap();
        assert_eq!(btc, "sudo -n systemctl disable --now xmrig && sudo -n systemctl enable --now cpuminer");
        assert!(switch_script(Coin::Monero, &m).is_err(), "no address yet");
        m.address = format!("4{}", "AbCd".repeat(23) + "ef");
        let xmr = switch_script(Coin::Monero, &m).unwrap();
        assert!(xmr.contains("XMRIG_POOL=%s"));
        assert!(xmr.contains(&format!("pool.supportxmr.com:443 {}", m.address)));
        assert!(xmr.contains("sudo -n tee /etc/default/xmrig"));
        assert!(xmr.ends_with("sudo -n systemctl disable --now cpuminer && sudo -n systemctl enable --now xmrig"));
        m.pool = "evil.example:443;reboot".into();
        assert!(switch_script(Coin::Monero, &m).is_err(), "pool is validated");
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
