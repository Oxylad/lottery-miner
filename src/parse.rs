//! Parsers for cpuminer output: the stdout log, the `summary` API reply and
//! the journal text of the home server's miner. Pure functions, unit tested.

use std::collections::HashMap;

/// Hash rate in hashes per second from strings like "68.35Mh/s" or "1.2Gh/s".
pub fn parse_hashrate(text: &str) -> Option<f64> {
    let t = text.trim().trim_matches(|c| c == '(' || c == ')');
    let t = t.strip_suffix("h/s")?;
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let value: f64 = t[..split].parse().ok()?;
    let mult = match &t[split..] {
        "" => 1.0,
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        _ => return None,
    };
    Some(value * mult)
}

/// Strips a leading "[YYYY-MM-DD HH:MM:SS] " timestamp if present.
pub fn strip_timestamp(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            return trimmed[end + 1..].trim_start();
        }
    }
    trimmed
}

/// Running counters and flags gathered from one miner's stdout log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogState {
    pub connected: bool,
    pub stall_count: u32,
    pub stratum_resets: u32,
    /// Last real speed from the summary block (bracketed number), H/s.
    pub hashrate: Option<f64>,
    pub submitted: u64,
    pub accepted: u64,
    pub difficulty: Option<f64>,
    pub block: Option<u64>,
    pub netdiff: Option<f64>,
    pub last_job: Option<String>,
    /// True once a stall line was seen and no new work has arrived since.
    pub stalled_now: bool,
}

impl LogState {
    pub fn rejected(&self) -> u64 {
        self.submitted.saturating_sub(self.accepted)
    }

    /// Feeds one stdout/stderr line into the state.
    pub fn feed(&mut self, raw: &str) {
        let line = strip_timestamp(raw);
        if line.starts_with("Stratum connection established") {
            self.connected = true;
        } else if line.starts_with("Nonce range exhausted, extranonce not subscribed") {
            self.stall_count += 1;
            self.stalled_now = true;
        } else if line.starts_with("Waiting for new work") {
            self.stalled_now = true;
        } else if line.starts_with("New Stratum Diff") {
            if let Some(d) = field_after(line, "New Stratum Diff ") {
                self.difficulty = d.trim_end_matches(',').parse().ok();
            }
            if let Some(b) = field_after(line, "Block ") {
                self.block = b.trim_end_matches(',').parse().ok();
            }
            self.last_job = field_after(line, "Job ").map(|s| s.to_string());
        } else if line.starts_with("New Work:") {
            self.stalled_now = false;
            if let Some(b) = field_after(line, "Block ") {
                self.block = b.trim_end_matches(',').parse().ok();
            }
            if let Some(n) = field_after(line, "Netdiff ") {
                self.netdiff = n.trim_end_matches(',').parse().ok();
            }
            self.last_job = field_after(line, "Job ").map(|s| s.to_string());
        } else if line.starts_with("Hash rate") {
            if let Some(hr) = bracketed_hashrate(line) {
                self.hashrate = Some(hr);
            }
        } else if line.starts_with("Submitted") {
            if let Some(n) = last_integer(line) {
                self.submitted = n;
            }
        } else if line.starts_with("Accepted") {
            if let Some(n) = last_integer(line) {
                self.accepted = n;
            }
        } else if line.starts_with("Stratum resets") {
            if let Some(n) = last_integer(line) {
                self.stratum_resets = n as u32;
            }
        }
    }
}

fn field_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let idx = line.find(key)? + key.len();
    line[idx..].split_whitespace().next()
}

/// "(68.35Mh/s)" from a "Hash rate" line, the real speed.
fn bracketed_hashrate(line: &str) -> Option<f64> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    parse_hashrate(&line[open + 1..close])
}

/// Last whitespace-separated token that parses as an integer.
fn last_integer(line: &str) -> Option<u64> {
    line.split_whitespace().rev().find_map(|t| t.parse().ok())
}

/// Parsed `summary` reply from the cpuminer API (`NAME=...;KHS=...|`).
#[derive(Debug, Clone, PartialEq)]
pub struct ApiSummary {
    pub hashrate: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub difficulty: Option<f64>,
    pub uptime_secs: Option<u64>,
    pub temp: Option<f64>,
}

pub fn parse_api_summary(reply: &str) -> Option<ApiSummary> {
    let segment = reply
        .split('|')
        .find(|s| s.contains("KHS=") || s.contains("HS="))?;
    let map: HashMap<&str, &str> = segment
        .split(';')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    let hashrate = if let Some(khs) = map.get("KHS").and_then(|v| v.parse::<f64>().ok()) {
        khs * 1000.0
    } else {
        map.get("HS")?.parse::<f64>().ok()?
    };
    let accepted = map.get("ACC")?.parse().ok()?;
    let rejected = map.get("REJ").and_then(|v| v.parse().ok()).unwrap_or(0);
    Some(ApiSummary {
        hashrate,
        accepted,
        rejected,
        difficulty: map.get("DIFF").and_then(|v| v.parse().ok()),
        uptime_secs: map.get("UPTIME").and_then(|v| v.parse().ok()),
        temp: map.get("TEMP").and_then(|v| v.parse().ok()),
    })
}

/// Latest-state summary of a journal dump from the home server.
pub fn parse_journal(lines: &str) -> LogState {
    let mut st = LogState::default();
    for line in lines.lines() {
        st.feed(line);
    }
    st
}

/// CPU package temperature from `sensors` output ("Package id 0: +52.0°C").
pub fn parse_package_temp(sensors: &str) -> Option<f64> {
    sensors.lines().find_map(|l| {
        if !l.contains("Package") {
            return None;
        }
        let plus = l.find('+')?;
        let rest = &l[plus + 1..];
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        rest[..end].parse().ok()
    })
}

/// Watts from two RAPL energy readings (microjoules) taken `secs` apart.
pub fn watts_from_energy(e1_uj: u64, e2_uj: u64, secs: f64) -> Option<f64> {
    if secs <= 0.0 || e2_uj < e1_uj {
        return None;
    }
    Some((e2_uj - e1_uj) as f64 / 1e6 / secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
[2026-11-13 22:27:48] Stratum connection established
[2026-11-13 22:27:48] New Stratum Diff 10000, Block 975715, Tx 12, Job 6af4689e00001b7f
                      TTF @ 285.96 Mh/s: Block 136y070d, Share 1d17h
[2026-11-13 22:27:58] Nonce range exhausted, extranonce not subscribed.
[2026-11-13 22:28:12] New Work: Block 975715, Tx 12, Netdiff 1.4836e+014, Job 6af4689e00001b80
                      Share rate        0.00/min     0.00/min
                      Hash rate          0.00h/s     44.34Mh/s   (68.35Mh/s)
                      Submitted             0            1
                      Accepted              0            1      100.0%
                      Stratum resets                     2
";

    #[test]
    fn hashrate_units() {
        assert_eq!(parse_hashrate("68.35Mh/s"), Some(68.35e6));
        assert_eq!(parse_hashrate("(1.2Gh/s)"), Some(1.2e9));
        assert_eq!(parse_hashrate("500h/s"), Some(500.0));
        assert_eq!(parse_hashrate("12Xh/s"), None);
        assert_eq!(parse_hashrate("junk"), None);
    }

    #[test]
    fn log_fixture_is_parsed() {
        let mut st = LogState::default();
        for l in LOG.lines() {
            st.feed(l);
        }
        assert!(st.connected);
        assert_eq!(st.stall_count, 1);
        assert_eq!(st.hashrate, Some(68.35e6));
        assert_eq!(st.submitted, 1);
        assert_eq!(st.accepted, 1);
        assert_eq!(st.rejected(), 0);
        assert_eq!(st.difficulty, Some(10000.0));
        assert_eq!(st.block, Some(975715));
        assert_eq!(st.netdiff, Some(1.4836e14));
        assert_eq!(st.last_job.as_deref(), Some("6af4689e00001b80"));
        assert!(!st.stalled_now, "new work clears the stall flag");
    }

    #[test]
    fn stall_flag_set_until_new_work() {
        let mut st = LogState::default();
        st.feed("[2026-11-13 22:27:58] Nonce range exhausted, extranonce not subscribed.");
        assert!(st.stalled_now);
        st.feed("[2026-11-13 22:28:00] Waiting for new work...");
        assert!(st.stalled_now);
        st.feed("[2026-11-13 22:28:12] New Work: Block 1, Tx 1, Netdiff 2, Job abc");
        assert!(!st.stalled_now);
    }

    #[test]
    fn api_summary_reply() {
        // Captured from cpuminer-opt 26.1 on this PC (2 threads, 41 s after start).
        let reply = "NAME=cpuminer-opt;VER=26.1;API=1.0;ALGO=410000;CPUS=2;URL=eusolo.ckpool.org:3333;HS=33626799.79;KHS=33626.80;ACC=0;REJ=0;SOL=0;ACCMN=0.000;DIFF=148364561003867.410000;TEMP=0.0;FAN=0;FREQ=0;UPTIME=41;TS=1794606909|";
        let s = parse_api_summary(reply).expect("parses");
        assert!((s.hashrate - 33_626_800.0).abs() < 1.0);
        assert_eq!(s.accepted, 0);
        assert_eq!(s.rejected, 0);
        assert_eq!(s.difficulty, Some(148364561003867.41));
        assert_eq!(s.uptime_secs, Some(41));
        assert!(parse_api_summary("").is_none());
    }

    #[test]
    fn journal_and_sensors_and_power() {
        let st = parse_journal(LOG);
        assert_eq!(st.hashrate, Some(68.35e6));
        let t = "coretemp-isa-0000\nPackage id 0:  +52.0°C  (high = +100.0°C)\n";
        assert_eq!(parse_package_temp(t), Some(52.0));
        assert_eq!(parse_package_temp("nothing"), None);
        // 82 W for 10 s = 820 J = 820_000_000 uJ
        assert_eq!(watts_from_energy(1_000_000_000, 1_820_000_000, 10.0), Some(82.0));
        assert_eq!(watts_from_energy(5, 1, 10.0), None);
    }
}
