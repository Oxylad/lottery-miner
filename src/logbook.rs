//! In-memory log of everything: each CPU instance, the GPU miner, app events
//! and the home server's journal. Capped so it never grows without bound.
//!
//! Every line carries one UTC timestamp set here (the journal's own time for
//! server lines, arrival time otherwise). The miners' own "[date time]"
//! prefixes are removed, because cpuminer writes local time and gpu-miner
//! writes UTC; the view sorts by the shared timestamp.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CAPACITY: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cpu(u32),
    Gpu,
    App,
    Server,
}

impl Source {
    pub fn label(self) -> String {
        match self {
            Source::Cpu(i) => format!("cpu#{}", i + 1),
            Source::Gpu => "gpu".into(),
            Source::App => "app".into(),
            Source::Server => "oxyhome".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub source: Source,
    /// Unix time in seconds (UTC), with fractions.
    pub at: f64,
    /// Insertion order, to keep lines with the same time in order.
    pub seq: u64,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct LogBook {
    lines: VecDeque<Line>,
    next_seq: u64,
    /// Newest server journal timestamp already added (journal -o short-unix).
    server_seen: f64,
}

pub type SharedLog = Arc<Mutex<LogBook>>;

pub fn shared() -> SharedLog {
    Arc::new(Mutex::new(LogBook::default()))
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Removes a leading "[YYYY-MM-DD HH:MM:SS] " that miners add themselves.
/// Leaves the indentation of continuation lines alone.
pub fn strip_own_timestamp(text: &str) -> &str {
    let b = text.as_bytes();
    let is_stamp = b.len() >= 21
        && b[0] == b'['
        && b[20] == b']'
        && b[5] == b'-'
        && b[8] == b'-'
        && b[11] == b' '
        && b[14] == b':'
        && b[17] == b':'
        && [1, 2, 3, 4, 6, 7, 9, 10, 12, 13, 15, 16, 18, 19]
            .iter()
            .all(|&i| b[i].is_ascii_digit());
    if is_stamp {
        text[21..].strip_prefix(' ').unwrap_or(&text[21..])
    } else {
        text
    }
}

/// Text as stored: own timestamp removed, and cpuminer's deep indent for
/// continuation lines (it lines up under the removed stamp) shortened to 2.
fn clean(text: &str) -> String {
    let t = strip_own_timestamp(text.trim_end());
    let indent = t.len() - t.trim_start().len();
    if indent >= 8 {
        format!("  {}", t.trim_start())
    } else {
        t.to_string()
    }
}

/// (year, month, day, hour, minute, second) in UTC.
fn civil_utc(at: f64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = at.floor() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// "HH:MM:SS" in UTC.
pub fn utc_time(at: f64) -> String {
    let (_, _, _, h, mi, s) = civil_utc(at);
    format!("{h:02}:{mi:02}:{s:02}")
}

/// "YYYY-MM-DD HH:MM:SS" in UTC.
pub fn utc_datetime(at: f64) -> String {
    let (y, m, d, h, mi, s) = civil_utc(at);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

impl LogBook {
    pub fn push(&mut self, source: Source, text: &str) {
        self.push_at(source, now(), text);
    }

    pub fn push_at(&mut self, source: Source, at: f64, text: &str) {
        let text = clean(text);
        if text.trim().is_empty() {
            return;
        }
        if self.lines.len() >= CAPACITY {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            source,
            at,
            seq: self.next_seq,
            text,
        });
        self.next_seq += 1;
    }

    /// Adds journal lines newer than the last ones seen, stamped with the
    /// journal's own time. `entries` are (unix time, message), oldest first.
    pub fn push_server(&mut self, entries: &[(f64, String)]) -> usize {
        let mut added = 0;
        for (ts, msg) in entries {
            if *ts > self.server_seen {
                self.push_at(Source::Server, *ts, msg);
                self.server_seen = *ts;
                added += 1;
            }
        }
        added
    }

    /// Newest first by timestamp, up to `limit`, matching `filter` (None = all).
    pub fn recent(&self, filter: Option<Filter>, limit: usize) -> Vec<Line> {
        let mut v: Vec<Line> = self
            .lines
            .iter()
            .filter(|l| filter.is_none_or(|f| f.matches(l.source)))
            .cloned()
            .collect();
        v.sort_by(|a, b| b.at.total_cmp(&a.at).then(b.seq.cmp(&a.seq)));
        v.truncate(limit);
        v
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Cpu,
    Gpu,
    App,
    Server,
}

impl Filter {
    pub fn matches(self, s: Source) -> bool {
        matches!(
            (self, s),
            (Filter::Cpu, Source::Cpu(_))
                | (Filter::Gpu, Source::Gpu)
                | (Filter::App, Source::App)
                | (Filter::Server, Source::Server)
        )
    }
}

/// Plain-text export, oldest first, with full UTC timestamps.
pub fn to_text(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines.iter().rev() {
        out.push_str(&format!(
            "{} UTC  {:<8} {}\n",
            utc_datetime(l.at),
            l.source.label(),
            l.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_filtered_newest_first() {
        let mut b = LogBook::default();
        for i in 0..(CAPACITY + 10) {
            b.push_at(Source::Cpu((i % 2) as u32), i as f64, &format!("line {i}"));
        }
        assert_eq!(b.len(), CAPACITY);
        b.push_at(Source::Gpu, 1e9, "gpu line");
        b.push(Source::App, "  ");
        let all = b.recent(None, 3);
        assert_eq!(all[0].text, "gpu line");
        assert_eq!(all.len(), 3);
        assert_eq!(b.recent(Some(Filter::Gpu), 10).len(), 1);
        let cpu = b.recent(Some(Filter::Cpu), 2);
        assert!(cpu.iter().all(|l| matches!(l.source, Source::Cpu(_))));
        assert_eq!(cpu[0].text, format!("line {}", CAPACITY + 9));
    }

    #[test]
    fn server_lines_are_not_duplicated_and_sort_into_place() {
        let mut b = LogBook::default();
        b.push_at(Source::Cpu(0), 11.5, "cpu between");
        let first = vec![(10.0, "a".to_string()), (11.0, "b".to_string())];
        assert_eq!(b.push_server(&first), 2);
        let second = vec![(11.0, "b".to_string()), (12.5, "c".to_string())];
        assert_eq!(b.push_server(&second), 1);
        let texts: Vec<_> = b.recent(None, 10).into_iter().map(|l| l.text).collect();
        // Sorted by time even though the server lines arrived later.
        assert_eq!(texts, vec!["c", "cpu between", "b", "a"]);
    }

    #[test]
    fn same_time_keeps_arrival_order() {
        let mut b = LogBook::default();
        b.push_at(Source::Cpu(0), 5.0, "first");
        b.push_at(Source::Cpu(0), 5.0, "second");
        let t: Vec<_> = b.recent(None, 10).into_iter().map(|l| l.text).collect();
        assert_eq!(t, vec!["second", "first"]);
    }

    #[test]
    fn miner_timestamps_are_removed() {
        // cpuminer (local time) and gpu-miner (UTC) prefixes both go.
        assert_eq!(
            strip_own_timestamp("[2026-11-14 00:44:41] API listening to 127.0.0.1:4048"),
            "API listening to 127.0.0.1:4048"
        );
        assert_eq!(
            strip_own_timestamp("[2026-11-13 23:44:45] Stratum connection established"),
            "Stratum connection established"
        );
        // Continuation lines and other brackets are kept as they are.
        assert_eq!(
            strip_own_timestamp("                      Hash rate   (68.72Mh/s)"),
            "                      Hash rate   (68.72Mh/s)"
        );
        assert_eq!(strip_own_timestamp("CPU affinity [!!!!....]"), "CPU affinity [!!!!....]");
        assert_eq!(strip_own_timestamp("[x] short"), "[x] short");
        // Stored form: continuation lines get a short indent.
        assert_eq!(clean("                      Hash rate   (68.72Mh/s)"), "  Hash rate   (68.72Mh/s)");
        assert_eq!(clean("[2026-11-14 00:44:41] API listening"), "API listening");
        assert_eq!(clean("    CPU: AMD Ryzen 7 7700X"), "    CPU: AMD Ryzen 7 7700X");
    }

    #[test]
    fn utc_formatting() {
        // 1794613114 = 2026-11-13 23:38:34 UTC (journal line from oxyhome).
        assert_eq!(utc_datetime(1794613114.204477), "2026-11-13 23:38:34");
        assert_eq!(utc_time(1794613114.9), "23:38:34");
        assert_eq!(utc_datetime(0.0), "1970-01-01 00:00:00");
    }

    #[test]
    fn text_export_is_oldest_first_with_utc() {
        let mut b = LogBook::default();
        b.push_at(Source::App, 1794613114.0, "first");
        b.push_at(Source::Gpu, 1794613115.0, "[2026-11-13 23:38:35] second");
        let t = to_text(&b.recent(None, 10));
        assert_eq!(
            t,
            "2026-11-13 23:38:34 UTC  app      first\n2026-11-13 23:38:35 UTC  gpu      second\n"
        );
    }
}
