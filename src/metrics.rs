//! Prometheus text format for the Grafana dashboard, pushed to a Pushgateway
//! (HTTPS + basic auth through nginx). Building the text is pure and tested.

use std::time::Duration;

use crate::settings::MetricsSettings;

/// One metric family: all samples of a name must be written together.
struct Family {
    name: String,
    kind: &'static str,
    help: &'static str,
    samples: Vec<String>,
}

/// Collects samples and writes them grouped per metric name.
#[derive(Default)]
pub struct Writer {
    families: Vec<Family>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn gauge(&mut self, name: &str, help: &'static str, labels: &[(&str, &str)], value: f64) {
        self.add(name, "gauge", help, labels, value);
    }

    pub fn counter(&mut self, name: &str, help: &'static str, labels: &[(&str, &str)], value: f64) {
        self.add(name, "counter", help, labels, value);
    }

    /// Adds a gauge only when the value is known.
    pub fn gauge_opt(&mut self, name: &str, help: &'static str, labels: &[(&str, &str)], value: Option<f64>) {
        if let Some(v) = value {
            self.gauge(name, help, labels, v);
        }
    }

    fn add(&mut self, name: &str, kind: &'static str, help: &'static str, labels: &[(&str, &str)], value: f64) {
        if !value.is_finite() {
            return;
        }
        let sample = format!("{name}{} {}", format_labels(labels), format_value(value));
        match self.families.iter_mut().find(|f| f.name == name) {
            Some(f) => f.samples.push(sample),
            None => self.families.push(Family {
                name: name.to_string(),
                kind,
                help,
                samples: vec![sample],
            }),
        }
    }

    pub fn finish(self) -> String {
        let mut out = String::new();
        for f in self.families {
            out.push_str(&format!("# HELP {} {}\n# TYPE {} {}\n", f.name, f.help, f.name, f.kind));
            for s in f.samples {
                out.push_str(&s);
                out.push('\n');
            }
        }
        out
    }
}

fn format_labels(labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect();
    format!("{{{}}}", inner.join(","))
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn format_value(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// RFC 4648 base64, for the basic auth header (no extra crate for 20 lines).
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn auth(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .redirects(0)
        .user_agent(concat!("lottery-miner/", env!("CARGO_PKG_VERSION")))
        .build()
}

fn status_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {}", body.chars().take(160).collect::<String>())
            }
        }
        other => other.to_string(),
    }
}

/// Basic auth plus the Cloudflare Access service token headers when set.
fn request(method: &str, m: &MetricsSettings) -> ureq::Request {
    let mut r = agent()
        .request(method, &m.group_url())
        .set("Authorization", &auth(&m.user, &m.password));
    if !m.cf_access_client_id.is_empty() && !m.cf_access_client_secret.is_empty() {
        r = r
            .set("CF-Access-Client-Id", m.cf_access_client_id.trim())
            .set("CF-Access-Client-Secret", m.cf_access_client_secret.trim());
    }
    r
}

/// Cloudflare Access answers a missing/invalid token with a redirect to its login.
fn check(resp: ureq::Response) -> Result<(), String> {
    if resp.get_url().contains("cloudflareaccess.com") || resp.status() >= 300 {
        return Err("blocked by Cloudflare Access (service token missing or not allowed)".into());
    }
    Ok(())
}

/// Replaces this PC's whole group on the Pushgateway.
pub fn push(m: &MetricsSettings, body: &str) -> Result<(), String> {
    request("PUT", m)
        .set("Content-Type", "text/plain; version=0.0.4")
        .send_string(body)
        .map_err(status_error)
        .and_then(check)
}

/// Removes this PC's group (on Quit), so Grafana shows it offline at once.
pub fn delete(m: &MetricsSettings) -> Result<(), String> {
    request("DELETE", m).call().map_err(status_error).and_then(check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_samples_per_family() {
        let mut w = Writer::new();
        w.gauge("lm_hashrate_hps", "Hashrate.", &[("coin", "btc"), ("miner", "gpu")], 2.0e9);
        w.gauge("lm_up", "Up.", &[], 1.0);
        w.gauge("lm_hashrate_hps", "Hashrate.", &[("coin", "xmr"), ("miner", "xmrig")], 1234.5);
        w.gauge_opt("lm_missing", "Never written.", &[], None);
        w.gauge("lm_nan", "Skipped.", &[], f64::NAN);
        w.counter("lm_shares_total", "Shares.", &[("pool", "a\"b\\c")], 3.0);
        let text = w.finish();
        assert_eq!(
            text,
            "# HELP lm_hashrate_hps Hashrate.\n# TYPE lm_hashrate_hps gauge\n\
             lm_hashrate_hps{coin=\"btc\",miner=\"gpu\"} 2000000000\n\
             lm_hashrate_hps{coin=\"xmr\",miner=\"xmrig\"} 1234.5\n\
             # HELP lm_up Up.\n# TYPE lm_up gauge\nlm_up 1\n\
             # HELP lm_shares_total Shares.\n# TYPE lm_shares_total counter\n\
             lm_shares_total{pool=\"a\\\"b\\\\c\"} 3\n"
        );
    }

    #[test]
    fn big_values_keep_exponent() {
        let mut w = Writer::new();
        w.gauge("lm_network_difficulty", "D.", &[], 1.4836e17);
        assert!(w.finish().contains("lm_network_difficulty 148360000000000000"));
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(auth("user", "pass"), "Basic dXNlcjpwYXNz");
    }
}
