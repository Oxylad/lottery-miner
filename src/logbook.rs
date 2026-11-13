//! In-memory log of everything: each CPU instance, the GPU miner, app events
//! and the home server's journal. Capped so it never grows without bound.

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
    /// Unix seconds when the app received the line.
    pub at: u64,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct LogBook {
    lines: VecDeque<Line>,
    /// Newest server journal timestamp already added (journal -o short-unix).
    server_seen: f64,
}

pub type SharedLog = Arc<Mutex<LogBook>>;

pub fn shared() -> SharedLog {
    Arc::new(Mutex::new(LogBook::default()))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl LogBook {
    pub fn push(&mut self, source: Source, text: &str) {
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        if self.lines.len() >= CAPACITY {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            source,
            at: now(),
            text: text.to_string(),
        });
    }

    /// Adds journal lines newer than the last ones seen. `entries` are
    /// (unix timestamp, message) in journal order.
    pub fn push_server(&mut self, entries: &[(f64, String)]) -> usize {
        let mut added = 0;
        for (ts, msg) in entries {
            if *ts > self.server_seen {
                self.push(Source::Server, msg);
                self.server_seen = *ts;
                added += 1;
            }
        }
        added
    }

    /// Newest first, up to `limit`, matching `filter` (None = all).
    pub fn recent(&self, filter: Option<Filter>, limit: usize) -> Vec<Line> {
        self.lines
            .iter()
            .rev()
            .filter(|l| filter.is_none_or(|f| f.matches(l.source)))
            .take(limit)
            .cloned()
            .collect()
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

/// Plain-text export, oldest first, for the clipboard.
pub fn to_text(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines.iter().rev() {
        out.push_str(&format!("{:<8} {}\n", l.source.label(), l.text));
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
            b.push(Source::Cpu((i % 2) as u32), &format!("line {i}"));
        }
        assert_eq!(b.len(), CAPACITY);
        b.push(Source::Gpu, "gpu line");
        b.push(Source::App, "  ");
        let all = b.recent(None, 3);
        assert_eq!(all[0].text, "gpu line");
        assert_eq!(all.len(), 3);
        let gpu = b.recent(Some(Filter::Gpu), 10);
        assert_eq!(gpu.len(), 1);
        let cpu = b.recent(Some(Filter::Cpu), 2);
        assert!(cpu.iter().all(|l| matches!(l.source, Source::Cpu(_))));
        assert_eq!(cpu[0].text, format!("line {}", CAPACITY + 9));
    }

    #[test]
    fn server_lines_are_not_duplicated() {
        let mut b = LogBook::default();
        let first = vec![(10.0, "a".to_string()), (11.0, "b".to_string())];
        assert_eq!(b.push_server(&first), 2);
        let second = vec![(11.0, "b".to_string()), (12.5, "c".to_string())];
        assert_eq!(b.push_server(&second), 1);
        let texts: Vec<_> = b.recent(Some(Filter::Server), 10).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, vec!["c", "b", "a"]);
    }

    #[test]
    fn text_export_is_oldest_first() {
        let mut b = LogBook::default();
        b.push(Source::App, "first");
        b.push(Source::Gpu, "second");
        let t = to_text(&b.recent(None, 10));
        assert_eq!(t, "app      first\ngpu      second\n");
    }
}
