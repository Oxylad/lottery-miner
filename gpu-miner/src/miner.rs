//! Stratum v1 client, mining loop and a cpuminer-compatible stats API.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::gpu::{self, Gpu};
use crate::sha256;
use crate::work::{self, Job};

pub struct Config {
    pub url: String,
    pub user: String,
    pub pass: String,
    pub api: Option<String>,
    pub suggest_diff: Option<f64>,
    pub verbose: bool,
    pub threads: u32,
    pub iter: u32,
}

#[derive(Default)]
struct PoolState {
    job: Option<Job>,
    extranonce1: Vec<u8>,
    extranonce2_size: usize,
    share_diff: f64,
    net_diff: f64,
    height: Option<u64>,
}

struct Shared {
    pool: Mutex<PoolState>,
    /// Bumped on every new job or extranonce change; the miner restarts work.
    generation: AtomicU64,
    connected: AtomicBool,
    writer: Mutex<Option<TcpStream>>,
    pending: Mutex<HashMap<u64, Instant>>,
    next_id: AtomicU64,
    hashes: AtomicU64,
    submitted: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
    rate_bits: AtomicU64,
    started: Instant,
    gpu_name: String,
    verbose: bool,
}

impl Shared {
    fn rate(&self) -> f64 {
        f64::from_bits(self.rate_bits.load(Ordering::Relaxed))
    }
}

/// "[YYYY-MM-DD HH:MM:SS] msg" in UTC, flushed, like cpuminer's log lines.
pub fn log(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let line = format!(
        "[{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}] {msg}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    );
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Block height from a BIP34 coinbase (best effort, for log lines only).
fn coinbase_height(coinb1: &str) -> Option<u64> {
    let b = work::hex_decode(coinb1).ok()?;
    // version(4) + input count(1) + prevout(36) + script length(1)
    let push = *b.get(42)? as usize;
    if !(1..=8).contains(&push) {
        return None;
    }
    let bytes = b.get(43..43 + push)?;
    Some(bytes.iter().rev().fold(0u64, |acc, x| acc * 256 + *x as u64))
}

fn send(shared: &Shared, msg: &Value) -> Result<(), String> {
    let mut guard = shared.writer.lock().map_err(|_| "lock")?;
    let stream = guard.as_mut().ok_or("not connected")?;
    let mut line = msg.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

fn host_port(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("stratum+tcp://")
        .ok_or("pool URL must start with stratum+tcp://")?;
    if !rest.contains(':') {
        return Err("pool URL needs a port".into());
    }
    Ok(rest.trim_end_matches('/').to_string())
}

fn handle_message(shared: &Shared, msg: &Value) {
    let method = msg.get("method").and_then(|m| m.as_str());
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match method {
        Some("mining.notify") => {
            let p = params.as_array().cloned().unwrap_or_default();
            let s = |i: usize| p.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let job = Job {
                job_id: s(0),
                prevhash: s(1),
                coinb1: s(2),
                coinb2: s(3),
                merkle_branch: p
                    .get(4)
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                version: s(5),
                nbits: s(6),
                ntime: s(7),
                clean: p.get(8).and_then(|v| v.as_bool()).unwrap_or(false),
            };
            let net = work::nbits_difficulty(&job.nbits).unwrap_or(0.0);
            let height = coinbase_height(&job.coinb1);
            if shared.verbose {
                if let Ok(p) = work::prevhash_from_stratum(&job.prevhash) {
                    log(&format!("Job {} prev block {}", job.job_id, work::display_hash(&p)));
                }
            }
            if let Ok(mut st) = shared.pool.lock() {
                st.net_diff = net;
                st.height = height;
                st.job = Some(job.clone());
            }
            shared.generation.fetch_add(1, Ordering::SeqCst);
            log(&format!(
                "New Work: Block {}, Netdiff {:.4e}, Job {}",
                height.map(|h| h.to_string()).unwrap_or_else(|| "?".into()),
                net,
                job.job_id
            ));
        }
        Some("mining.set_difficulty") => {
            if let Some(d) = params.get(0).and_then(|v| v.as_f64()) {
                if let Ok(mut st) = shared.pool.lock() {
                    st.share_diff = d;
                }
                log(&format!("New Stratum Diff {d}, Job -"));
            }
        }
        Some("client.reconnect") => {
            log("Pool asked to reconnect");
            if let Ok(mut w) = shared.writer.lock() {
                if let Some(s) = w.take() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        Some(_) => {}
        None => {
            // A reply. Submits use ids >= 100.
            let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            if id >= 100 {
                let sent = shared.pending.lock().ok().and_then(|mut p| p.remove(&id));
                let ok = msg.get("result").and_then(|v| v.as_bool()).unwrap_or(false);
                if ok {
                    shared.accepted.fetch_add(1, Ordering::Relaxed);
                } else {
                    shared.rejected.fetch_add(1, Ordering::Relaxed);
                }
                let acc = shared.accepted.load(Ordering::Relaxed);
                let total = acc + shared.rejected.load(Ordering::Relaxed);
                let ms = sent.map(|t| t.elapsed().as_millis()).unwrap_or(0);
                if ok {
                    log(&format!("Accepted {acc}/{total}, {ms} ms"));
                } else {
                    log(&format!("Rejected {acc}/{total}: {}", msg.get("error").cloned().unwrap_or(Value::Null)));
                }
            }
        }
    }
}

/// Connects, subscribes, authorizes and reads until the connection drops.
fn stratum_session(shared: &Arc<Shared>, cfg: &Config) -> Result<(), String> {
    let hp = host_port(&cfg.url)?;
    // Try every address, IPv4 first (some pool hosts publish IPv6 they refuse on).
    let mut addrs: Vec<_> = hp.to_socket_addrs().map_err(|e| format!("{hp}: {e}"))?.collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    let mut last_err = format!("{hp}: no address");
    let mut connected = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            Ok(s) => {
                connected = Some(s);
                break;
            }
            Err(e) => last_err = format!("{hp} ({addr}): {e}"),
        }
    }
    let stream = connected.ok_or(last_err)?;
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .map_err(|e| e.to_string())?;
    *shared.writer.lock().map_err(|_| "lock")? = Some(stream.try_clone().map_err(|e| e.to_string())?);
    let mut reader = BufReader::new(stream);

    send(shared, &json!({"id": 1, "method": "mining.subscribe", "params": ["gpu-miner/0.1"]}))?;
    let mut line = String::new();
    // Subscribe reply (notifications may come first on some pools).
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("pool closed the connection".into());
        }
        let msg: Value = serde_json::from_str(line.trim()).map_err(|e| format!("bad JSON: {e}"))?;
        if msg.get("id").and_then(|v| v.as_u64()) == Some(1) {
            let r = msg.get("result").and_then(|v| v.as_array()).ok_or("subscribe failed")?;
            let en1 = r.get(1).and_then(|v| v.as_str()).ok_or("no extranonce1")?;
            let size = r.get(2).and_then(|v| v.as_u64()).ok_or("no extranonce2 size")?;
            let mut st = shared.pool.lock().map_err(|_| "lock")?;
            st.extranonce1 = work::hex_decode(en1)?;
            st.extranonce2_size = size as usize;
            st.job = None;
            log(&format!("Stratum extranonce1 0x{en1}, extranonce2 size {size}"));
            break;
        }
        handle_message(shared, &msg);
    }
    send(shared, &json!({"id": 2, "method": "mining.authorize", "params": [cfg.user, cfg.pass]}))?;
    if let Some(d) = cfg.suggest_diff {
        send(shared, &json!({"id": 3, "method": "mining.suggest_difficulty", "params": [d]}))?;
    }
    shared.connected.store(true, Ordering::SeqCst);
    log("Stratum connection established");
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("pool closed the connection".into());
        }
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if msg.get("id").and_then(|v| v.as_u64()) == Some(2)
            && msg.get("result").and_then(|v| v.as_bool()) == Some(false)
        {
            return Err(format!("authorization failed for {}", cfg.user));
        }
        handle_message(shared, &msg);
    }
}

fn stratum_loop(shared: Arc<Shared>, cfg: Arc<Config>) {
    let mut backoff = 5;
    loop {
        let started = Instant::now();
        let res = stratum_session(&shared, &cfg);
        shared.connected.store(false, Ordering::SeqCst);
        if let Ok(mut st) = shared.pool.lock() {
            st.job = None;
        }
        shared.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut w) = shared.writer.lock() {
            *w = None;
        }
        log(&format!(
            "Stratum connection interrupted: {}",
            res.err().unwrap_or_default()
        ));
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 5;
        }
        thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(60);
    }
}

fn summary(shared: &Shared, url: &str) -> String {
    let st = shared.pool.lock();
    let net = st.as_ref().map(|s| s.net_diff).unwrap_or(0.0);
    let rate = shared.rate();
    let acc = shared.accepted.load(Ordering::Relaxed);
    let up = shared.started.elapsed().as_secs();
    let accmn = if up > 0 { acc as f64 * 60.0 / up as f64 } else { 0.0 };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "NAME=gpu-miner;VER={};API=1.0;ALGO=sha256d;GPU={};URL={};HS={:.2};KHS={:.2};ACC={};REJ={};SOL=0;ACCMN={:.3};DIFF={:.6};TEMP=0.0;FAN=0;FREQ=0;UPTIME={};TS={}|",
        env!("CARGO_PKG_VERSION"),
        shared.gpu_name.replace([';', '|', '='], " "),
        url.trim_start_matches("stratum+tcp://"),
        rate,
        rate / 1000.0,
        acc,
        shared.rejected.load(Ordering::Relaxed),
        accmn,
        net,
        up,
        ts
    )
}

fn api_loop(listener: TcpListener, shared: Arc<Shared>, url: String) {
    for conn in listener.incoming() {
        let Ok(mut s) = conn else { continue };
        s.set_read_timeout(Some(Duration::from_millis(500))).ok();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).unwrap_or(0);
        let cmd = String::from_utf8_lossy(&buf[..n]).trim().trim_end_matches('|').to_string();
        let reply = match cmd.as_str() {
            "summary" | "" => summary(&shared, &url),
            "quit" => {
                let _ = s.write_all(b"bye|");
                log("API quit received");
                std::process::exit(0);
            }
            _ => "STATUS=E;MSG=unknown command|".to_string(),
        };
        let _ = s.write_all(reply.as_bytes());
    }
}

/// Runs forever: mines the current job, rolling extranonce2.
pub fn run(gpu: Gpu, cfg: Config) -> Result<(), String> {
    let shared = Arc::new(Shared {
        pool: Mutex::new(PoolState::default()),
        generation: AtomicU64::new(0),
        connected: AtomicBool::new(false),
        writer: Mutex::new(None),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(100),
        hashes: AtomicU64::new(0),
        submitted: AtomicU64::new(0),
        accepted: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        rate_bits: AtomicU64::new(0f64.to_bits()),
        started: Instant::now(),
        gpu_name: gpu.name.clone(),
        verbose: cfg.verbose,
    });
    let cfg = Arc::new(cfg);
    if let Some(api) = &cfg.api {
        let listener = TcpListener::bind(api).map_err(|e| format!("API {api}: {e}"))?;
        log(&format!("API listening to {api}"));
        let (s, u) = (shared.clone(), cfg.url.clone());
        thread::spawn(move || api_loop(listener, s, u));
    }
    log(&format!("Stratum connect {}", cfg.url));
    {
        let (s, c) = (shared.clone(), cfg.clone());
        thread::spawn(move || stratum_loop(s, c));
    }

    let per_dispatch = (cfg.threads.div_ceil(gpu::WORKGROUP) * gpu::WORKGROUP) as u64 * cfg.iter as u64;
    let mut en2_counter: u64 = 0;
    let mut rate_t = Instant::now();
    let mut rate_h = 0u64;
    let mut summary_t = Instant::now();
    loop {
        // Snapshot the current job.
        let job_gen = shared.generation.load(Ordering::SeqCst);
        let snap = {
            let st = shared.pool.lock().map_err(|_| "lock")?;
            st.job.clone().map(|j| (j, st.extranonce1.clone(), st.extranonce2_size, st.share_diff, st.net_diff))
        };
        let Some((job, en1, en2_size, share_diff, net_diff)) = snap else {
            thread::sleep(Duration::from_millis(100));
            continue;
        };
        if en2_size == 0 || en2_size > 8 {
            return Err(format!("unsupported extranonce2 size {en2_size}"));
        }
        en2_counter = en2_counter.wrapping_add(1);
        let en2: Vec<u8> = en2_counter.to_be_bytes()[8 - en2_size..].to_vec();
        let root = work::merkle_root(&job, &en1, &en2)?;
        let header = work::header_with_root(&job, &root)?;
        let w = work::gpu_work(&header);

        let mut base: u64 = 0;
        while base < (1u64 << 32) && shared.generation.load(Ordering::SeqCst) == job_gen {
            let found = gpu.search(&w, base as u32, cfg.threads, cfg.iter)?;
            base += per_dispatch;
            shared.hashes.fetch_add(per_dispatch, Ordering::Relaxed);
            for nonce in found {
                let mut h = header;
                h[76..80].copy_from_slice(&nonce.to_le_bytes());
                let hash = sha256::sha256d(&h);
                let d = work::hash_difficulty(&hash);
                if d >= share_diff.max(1.0) {
                    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
                    let msg = json!({
                        "id": id,
                        "method": "mining.submit",
                        "params": [cfg.user, job.job_id, work::hex_encode(&en2), job.ntime, format!("{nonce:08x}")]
                    });
                    if let Ok(mut p) = shared.pending.lock() {
                        p.insert(id, Instant::now());
                    }
                    shared.submitted.fetch_add(1, Ordering::Relaxed);
                    if net_diff > 0.0 && d >= net_diff {
                        log(&format!("BLOCK CANDIDATE: share diff {d:.0} >= network {net_diff:.4e}, hash {}", work::display_hash(&hash)));
                    }
                    log(&format!("Submitting share diff {d:.1} (target {share_diff})"));
                    if let Err(e) = send(&shared, &msg) {
                        log(&format!("Submit failed: {e}"));
                    }
                }
            }
            // Hash rate over ~5 s windows, smoothed.
            let el = rate_t.elapsed().as_secs_f64();
            if el >= 5.0 {
                let total = shared.hashes.load(Ordering::Relaxed);
                let now_rate = (total - rate_h) as f64 / el;
                let old = shared.rate();
                let new = if old == 0.0 { now_rate } else { old * 0.6 + now_rate * 0.4 };
                shared.rate_bits.store(new.to_bits(), Ordering::Relaxed);
                rate_h = total;
                rate_t = Instant::now();
            }
            if summary_t.elapsed() >= Duration::from_secs(60) {
                summary_t = Instant::now();
                let r = shared.rate();
                let acc = shared.accepted.load(Ordering::Relaxed);
                let sub = shared.submitted.load(Ordering::Relaxed);
                println!("                      Hash rate          ({:.2}Mh/s)", r / 1e6);
                println!("                      Submitted          {sub}");
                println!("                      Accepted           {acc}");
                let _ = std::io::stdout().flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bip34_height() {
        // version 2, 1 input, null prevout, script len 0x2c, push 3: 0x0ee3d3 = 975827 LE d3e30e
        let coinb1 = format!("02000000{}{}{}{}", "01", "00".repeat(32), "ffffffff", "2c03d3e30e");
        assert_eq!(coinbase_height(&coinb1), Some(975827));
        assert_eq!(coinbase_height("00"), None);
    }

    #[test]
    fn url_parsing() {
        assert_eq!(host_port("stratum+tcp://eusolo.ckpool.org:3333").unwrap(), "eusolo.ckpool.org:3333");
        assert!(host_port("http://x:1").is_err());
        assert!(host_port("stratum+tcp://x").is_err());
    }
}
