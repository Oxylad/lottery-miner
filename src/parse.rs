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
    /// Last fatal/startup error line (gpu-miner), shown when the process exits.
    pub last_error: Option<String>,
    /// Jobs (new work) received from the pool, and when the last one came (unix s).
    pub jobs: u64,
    pub last_job_unix: Option<f64>,
    /// Real difficulty of the last share this miner submitted and the best one
    /// since it started (share difficulty units, 1 = 2^32 hashes on Bitcoin).
    pub last_share_diff: Option<f64>,
    pub best_share_diff: Option<f64>,
    pub last_share_unix: Option<f64>,
    /// Shares at or above network difficulty (a block).
    pub blocks_found: u64,
}

/// Unix time now in seconds.
pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl LogState {
    pub fn rejected(&self) -> u64 {
        self.submitted.saturating_sub(self.accepted)
    }

    fn job_seen(&mut self) {
        self.jobs += 1;
        self.last_job_unix = Some(now_unix());
    }

    fn share_seen(&mut self, diff: f64) {
        self.last_share_diff = Some(diff);
        self.last_share_unix = Some(now_unix());
        if self.best_share_diff.is_none_or(|b| diff > b) {
            self.best_share_diff = Some(diff);
        }
    }

    /// Feeds one stdout/stderr line into the state.
    pub fn feed(&mut self, raw: &str) {
        let line = strip_timestamp(raw);
        if self.feed_xmrig(line) {
            return;
        }
        if line.starts_with("fatal:")
            || line.starts_with("GPU init failed")
            || line.starts_with("selftest FAILED")
            || line.starts_with("Stratum connection interrupted")
        {
            self.last_error = Some(line.to_string());
            if line.starts_with("Stratum connection interrupted") {
                self.connected = false;
            }
        } else if line.starts_with("Stratum connection established") {
            self.last_error = None;
            self.connected = true;
        } else if line.starts_with("Nonce range exhausted, extranonce not subscribed") {
            self.stall_count += 1;
            self.stalled_now = true;
        } else if line.starts_with("Waiting for new work") {
            self.stalled_now = true;
        } else if let Some(d) = share_diff_of(line) {
            self.share_seen(d);
        } else if line.contains("BLOCK SOLVED") || line.starts_with("BLOCK CANDIDATE") {
            self.blocks_found += 1;
        } else if line.starts_with("New Stratum Diff") {
            if let Some(d) = field_after(line, "New Stratum Diff ") {
                self.difficulty = d.trim_end_matches(',').parse().ok();
            }
            if let Some(b) = field_after(line, "Block ") {
                self.block = b.trim_end_matches(',').parse().ok();
            }
            self.last_job = field_after(line, "Job ").map(|s| s.to_string());
            if self.last_job.as_deref().is_some_and(|j| j != "-") {
                self.job_seen();
            }
        } else if line.starts_with("New Work:") || line.starts_with("New Block ") {
            self.stalled_now = false;
            self.job_seen();
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
        } else if let Some((acc, total)) = share_tally(line) {
            // gpu-miner per-share verdict: "Accepted 1305/1306, 40 ms". The
            // last number there is the reply time, not a count.
            self.accepted = acc;
            self.submitted = self.submitted.max(total);
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

impl LogState {
    /// XMRig lines ("net      new job from ...", "miner    speed 10s/60s/15m ...",
    /// "cpu      accepted (3/0) diff ..."). Returns true if the line was one.
    fn feed_xmrig(&mut self, line: &str) -> bool {
        if let Some(i) = line.find("speed 10s/60s/15m") {
            // First number after the label is the 10 s average in H/s ("n/a" early on).
            let rest = &line[i + "speed 10s/60s/15m".len()..];
            if let Some(v) = rest.split_whitespace().next().and_then(|t| t.parse::<f64>().ok()) {
                self.hashrate = Some(v);
            }
            return true;
        }
        if let Some(i) = line.find(" accepted (").or_else(|| line.find(" rejected (")) {
            let rest = &line[i..];
            if let (Some(a), Some(b)) = (rest.find('('), rest.find(')')) {
                if let Some((acc, rej)) = rest[a + 1..b].split_once('/') {
                    if let (Ok(acc), Ok(rej)) = (acc.trim().parse::<u64>(), rej.trim().parse::<u64>()) {
                        self.accepted = acc;
                        self.submitted = acc + rej;
                    }
                }
            }
            // XMRig logs the difficulty the pool credits (its target), not the
            // share's real one; the real best comes from its HTTP API.
            if rest.starts_with(" accepted (") {
                if let Some(d) = field_after(rest, ") diff ").and_then(|d| d.parse().ok()) {
                    self.share_seen(d);
                }
            }
            return true;
        }
        if line.contains("new job from ") {
            self.job_seen();
            self.connected = true;
            self.last_error = None;
            if let Some(d) = field_after(line, "diff ") {
                self.difficulty = d.parse().ok();
            }
            if let Some(h) = field_after(line, "height ") {
                self.block = h.parse().ok();
            }
            return true;
        }
        if line.contains("use pool ") {
            self.connected = true;
            self.last_error = None;
            return true;
        }
        if line.contains(" error: ") && line.starts_with("net") {
            self.last_error = Some(line.split_whitespace().collect::<Vec<_>>().join(" "));
            return true;
        }
        false
    }
}

/// XMRig's HTTP API (GET /2/summary) reply: hashrate.total = [10s, 60s, 15m]
/// (null until measured), results.shares_good / shares_total, uptime.
pub fn parse_xmrig_summary(json: &str) -> Option<ApiSummary> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let totals = v.get("hashrate")?.get("total")?.as_array()?;
    let hashrate = totals.iter().find_map(|x| x.as_f64()).unwrap_or(0.0);
    let results = v.get("results");
    let good = results.and_then(|r| r.get("shares_good")).and_then(|x| x.as_u64()).unwrap_or(0);
    let total = results.and_then(|r| r.get("shares_total")).and_then(|x| x.as_u64()).unwrap_or(good);
    Some(ApiSummary {
        hashrate,
        accepted: good,
        rejected: total.saturating_sub(good),
        difficulty: results.and_then(|r| r.get("diff_current")).and_then(|x| x.as_f64()),
        uptime_secs: v.get("uptime").and_then(|x| x.as_u64()),
        temp: None,
        best_share: results
            .and_then(|r| r.get("best"))
            .and_then(|b| b.as_array())
            .and_then(|b| b.iter().filter_map(|x| x.as_f64()).reduce(f64::max))
            .filter(|b| *b > 0.0),
        device: v
            .get("cpu")
            .and_then(|c| c.get("brand"))
            .and_then(|b| b.as_str())
            .map(|s| s.trim().to_string()),
    })
}

/// Difficulty of a submitted share: cpuminer-opt "1 Submitted Diff 0.0234, Block ..."
/// or gpu-miner "Submitting share diff 12.3 (target 10000)".
fn share_diff_of(line: &str) -> Option<f64> {
    let rest = if let Some(i) = line.find("Submitted Diff ") {
        &line[i + "Submitted Diff ".len()..]
    } else if let Some(r) = line.strip_prefix("Submitting share diff ") {
        r
    } else {
        return None;
    };
    rest.split([',', ' ']).next()?.parse().ok()
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

/// (accepted, total) from gpu-miner's "Accepted 5/6, 40 ms" or "Rejected 5/6: ..." lines.
fn share_tally(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("Accepted ").or_else(|| line.strip_prefix("Rejected "))?;
    let token = rest.split([',', ':', ' ']).next()?;
    let (a, t) = token.split_once('/')?;
    Some((a.parse().ok()?, t.parse().ok()?))
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
    /// Device name (gpu-miner sends GPU=...).
    pub device: Option<String>,
    /// Best share difficulty since start (XMRig results.best).
    pub best_share: Option<f64>,
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
        device: map.get("GPU").map(|v| v.to_string()),
        best_share: None,
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
    fn shares_jobs_and_blocks() {
        let mut st = LogState::default();
        st.feed("[2026-11-18 10:00:00] New Stratum Diff 10000, Block 975715, Tx 12, Job 6af4");
        st.feed("[2026-11-18 10:00:01] New Work: Block 975715, Tx 12, Netdiff 1.4836e+014, Job 6af5");
        st.feed("[2026-11-18 10:00:02] New Block 975716, Tx 1, Netdiff 1.4836e+014, Job 6af6");
        assert_eq!(st.jobs, 3);
        assert!(st.last_job_unix.is_some());
        st.feed("[2026-11-18 10:01:00] 1 Submitted Diff 12345, Block 975716, Job 6af6");
        st.feed("[2026-11-18 10:02:00] 2 Submitted Diff 1.5e+04, Block 975716, Job 6af6");
        st.feed("[2026-11-18 10:03:00] 3 Submitted Diff 10001, Block 975716, Job 6af6");
        assert_eq!(st.last_share_diff, Some(10001.0));
        assert_eq!(st.best_share_diff, Some(15000.0));
        st.feed("[2026-11-18 10:04:00] Submitting share diff 22000.5 (target 10000)");
        assert_eq!(st.best_share_diff, Some(22000.5));
        st.feed("[2026-11-18 10:05:00] BLOCK CANDIDATE: share diff 2e14 >= network 1.4e14, hash 00");
        st.feed("[2026-11-18 10:05:00] 1 BLOCK SOLVED 975716");
        assert_eq!(st.blocks_found, 2);
        // gpu-miner's diff-only line carries no job.
        st.feed("[2026-11-18 10:06:00] New Stratum Diff 10000, Job -");
        assert_eq!(st.jobs, 3);
        st.feed("[2026-11-18 10:06:01] net      new job from pool.supportxmr.com:443 diff 120001 algo rx/0 height 3500000");
        assert_eq!(st.jobs, 4);
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
    fn xmrig_log_lines() {
        // Format of XMRig 6.x log lines (--no-color).
        let mut st = LogState::default();
        for l in [
            "[2026-11-16 17:00:00.123]  net      use pool pool.supportxmr.com:443 TLSv1.3 141.95.72.59",
            "[2026-11-16 17:00:00.456]  net      new job from pool.supportxmr.com:443 diff 120001 algo rx/0 height 3807757",
            "[2026-11-16 17:01:00.000]  miner    speed 10s/60s/15m 16234.5 16100.0 n/a H/s max 16300.0 H/s",
            "[2026-11-16 17:02:00.000]  cpu      accepted (3/1) diff 120001 (45 ms)",
        ] {
            st.feed(l);
        }
        assert!(st.connected);
        assert_eq!(st.block, Some(3807757));
        assert_eq!(st.difficulty, Some(120001.0));
        assert_eq!(st.hashrate, Some(16234.5));
        assert_eq!(st.accepted, 3);
        assert_eq!(st.rejected(), 1);
        assert_eq!(st.last_share_diff, Some(120001.0));
        assert!(st.last_share_unix.is_some());
        st.feed("[2026-11-16 17:02:30.000]  cpu      rejected (3/2) diff 150000 \"Low difficulty share\" (40 ms)");
        assert_eq!(st.last_share_diff, Some(120001.0));
        // Speed not measured yet leaves the old value alone.
        st.feed("[2026-11-16 17:03:00.000]  miner    speed 10s/60s/15m n/a n/a n/a H/s max n/a H/s");
        assert_eq!(st.hashrate, Some(16234.5));
        st.feed("[2026-11-16 17:04:00.000]  net      pool.supportxmr.com:443 error: \"connect error: timed out\"");
        assert!(st.last_error.unwrap().contains("connect error"));
    }

    #[test]
    fn gpu_miner_share_verdict_lines() {
        let mut st = LogState::default();
        st.feed("                      Submitted          1306");
        st.feed("                      Accepted           1290");
        st.feed("[2026-11-20 14:00:00] Accepted 1305/1306, 40 ms");
        assert_eq!(st.accepted, 1305);
        assert_eq!(st.rejected(), 1);
        st.feed("[2026-11-20 14:00:05] Rejected 1305/1307: [23,\"Stale\",null]");
        assert_eq!(st.accepted, 1305);
        assert_eq!(st.rejected(), 2);
    }

    #[test]
    fn xmrig_api_summary() {
        // Trimmed GET /2/summary reply from XMRig 6.x.
        let json = r#"{"id":"abc","worker_id":"desktop","uptime":125,"version":"6.26.0",
            "cpu":{"brand":"AMD Ryzen 7 7700X 8-Core Processor            "},
            "results":{"diff_current":120001,"shares_good":7,"shares_total":8,"avg_time":17},
            "hashrate":{"total":[16234.5,16100.0,null],"highest":16300.0}}"#;
        let s = parse_xmrig_summary(json).unwrap();
        assert_eq!(s.hashrate, 16234.5);
        assert_eq!(s.accepted, 7);
        assert_eq!(s.rejected, 1);
        assert_eq!(s.uptime_secs, Some(125));
        assert_eq!(s.device.as_deref(), Some("AMD Ryzen 7 7700X 8-Core Processor"));
        // Before the first measurement every total is null.
        let early = r#"{"uptime":3,"hashrate":{"total":[null,null,null]},"results":{"shares_good":0,"shares_total":0}}"#;
        assert_eq!(parse_xmrig_summary(early).unwrap().hashrate, 0.0);
        assert!(parse_xmrig_summary("not json").is_none());
    }

    #[test]
    fn gpu_miner_output() {
        // Captured from gpu-miner 0.1.0 on this PC.
        let reply = "NAME=gpu-miner;VER=0.1.0;API=1.0;ALGO=sha256d;GPU=AMD Radeon RX 7700 XT;URL=eusolo.ckpool.org:3333;HS=2017746058.72;KHS=2017746.06;ACC=0;REJ=0;SOL=0;ACCMN=0.000;DIFF=148364561003867.406250;TEMP=0.0;FAN=0;FREQ=0;UPTIME=149;TS=1794612170|";
        let s = parse_api_summary(reply).unwrap();
        assert!((s.hashrate / 1e9 - 2.0177).abs() < 1e-3);
        assert_eq!(s.device.as_deref(), Some("AMD Radeon RX 7700 XT"));

        let mut st = LogState::default();
        for l in [
            "[2026-11-13 23:20:21] Stratum connection established",
            "[2026-11-13 23:20:21] New Work: Block 975724, Netdiff 1.4836e14, Job 6af4689e00001c69",
            "                      Hash rate          (1914.29Mh/s)",
            "                      Submitted          0",
            "                      Accepted           0",
        ] {
            st.feed(l);
        }
        assert!(st.connected);
        assert_eq!(st.block, Some(975724));
        assert_eq!(st.netdiff, Some(1.4836e14));
        assert_eq!(st.hashrate, Some(1914.29e6));
        st.feed("[2026-11-13 23:25:00] Stratum connection interrupted: pool closed the connection");
        assert!(!st.connected);
        assert!(st.last_error.as_deref().unwrap().contains("pool closed"));
        st.feed("[2026-11-13 23:25:05] Stratum connection established");
        assert!(st.last_error.is_none());
        st.feed("GPU init failed: no matching GPU adapter (try --list-devices)");
        assert!(st.last_error.as_deref().unwrap().starts_with("GPU init failed"));
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
