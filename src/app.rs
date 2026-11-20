//! Dashboard state, the refresh loops and the actions the UI and tray call.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext, AsyncApp, Context, Entity, WeakEntity};

use crate::input::TextInput;
use crate::miner::{self, InstanceHandle, Miner};
use crate::network::{self, MoneroNet, Network, PoolUser, XmrPool};
use crate::remote::{self, OxyhomeStats, ServiceAction};
use crate::settings::{self, Coin, IdleAction, Settings};
use crate::logbook::{self, Filter, SharedLog, Source};
use crate::win;

/// Pool and mempool numbers change slowly; no need to ask more often.
const NETWORK_EVERY: Duration = Duration::from_secs(60);
/// The oxyhome probe itself takes ~10 s (energy sampling).
const OXYHOME_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default)]
pub struct InstanceView {
    pub index: u32,
    pub port: u16,
    pub hashrate: Option<f64>,
    /// True when the hashrate came from the API rather than the log.
    pub from_api: bool,
    pub accepted: u64,
    pub rejected: u64,
    pub uptime: Duration,
    pub stall_count: u32,
    pub stalled_now: bool,
    pub api_error: Option<String>,
    pub netdiff: Option<f64>,
    pub connected: bool,
    /// GPU name reported by gpu-miner.
    pub device: Option<String>,
    pub last_error: Option<String>,
    pub stratum_resets: u32,
    /// Pool share difficulty (stratum target) and the block of the current job.
    pub pool_diff: Option<f64>,
    pub block: Option<u64>,
    pub jobs: u64,
    pub last_job_unix: Option<f64>,
    pub last_share_diff: Option<f64>,
    pub best_share_diff: Option<f64>,
    pub last_share_unix: Option<f64>,
    pub blocks_found: u64,
}

pub struct Dashboard {
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub miner: Miner,
    pub local: Vec<InstanceView>,
    /// The GPU miner process (gpu-miner.exe), separate from the CPU instances.
    pub gpu_miner: Miner,
    pub gpu: Option<InstanceView>,
    /// Seconds since the last keyboard/mouse input.
    pub idle_secs: u64,
    /// Miners that idle mode started (and will stop on input).
    pub idle_cpu: bool,
    pub idle_gpu: bool,
    /// Idle mode already tried to start in this idle period.
    idle_tried: bool,
    idle_awake: bool,
    pub pool: Option<Result<Option<PoolUser>, String>>,
    pub network: Option<Result<Network, String>>,
    pub xmr_net: Option<Result<MoneroNet, String>>,
    /// Last (time, height) of the server's Monero node and its sync speed (blocks/s).
    node_sample: Option<(Instant, u64)>,
    pub node_rate: Option<f64>,
    /// Coin of the CPU miners that are running (or last ran).
    pub cpu_coin: Coin,
    pub oxyhome: Option<Result<OxyhomeStats, String>>,
    pub oxyhome_busy: bool,
    pub oxyhome_due: Instant,
    pub network_due: Instant,
    pub message: Option<String>,
    /// Everything the miners, the server and the app logged (Logs tab).
    pub logbook: SharedLog,
    pub view: View,
    pub log_filter: Option<Filter>,
    last_server_error: Option<String>,
    pub hwnd: Option<isize>,
    pub form: Form,
    /// SupportXMR stats for the Monero address.
    pub xmr_pool: Option<Result<Option<XmrPool>, String>>,
    started_unix: f64,
    /// Last metrics push: Ok(unix time) or the error.
    pub metrics_status: Option<Result<f64, String>>,
    metrics_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Settings,
    Logs,
}

/// Text fields of the settings page.
pub struct Form {
    pub miner_exe: Entity<TextInput>,
    pub address: Entity<TextInput>,
    pub pool_url: Entity<TextInput>,
    pub worker: Entity<TextInput>,
    pub ssh_name: Entity<TextInput>,
    pub ssh_host: Entity<TextInput>,
    pub ssh_port: Entity<TextInput>,
    pub ssh_key: Entity<TextInput>,
    pub gpu_worker: Entity<TextInput>,
    pub gpu_device: Entity<TextInput>,
    pub xmr_address: Entity<TextInput>,
    pub xmr_pool: Entity<TextInput>,
    pub xmrig_exe: Entity<TextInput>,
}

impl Form {
    fn new(s: &Settings, cx: &mut Context<Dashboard>) -> Self {
        let mut field = |text: &str, placeholder: &str| {
            let (t, p) = (text.to_string(), placeholder.to_string());
            cx.new(move |cx| TextInput::new(cx, &t, &p))
        };
        Self {
            miner_exe: field(&s.miner_exe, r"C:\cpuminer\cpuminer-avx512-sha-vaes.exe"),
            address: field(&s.address, "bc1q..."),
            pool_url: field(&s.pool_url, "stratum+tcp://eusolo.ckpool.org:3333"),
            worker: field(&s.worker, "desktop"),
            ssh_name: field(&s.oxyhome.name, "Home server"),
            ssh_host: field(&s.oxyhome.host, "user@hostname"),
            ssh_port: field(
                &s.oxyhome.port.map(|p| p.to_string()).unwrap_or_default(),
                "22",
            ),
            ssh_key: field(&s.oxyhome.key_file, "~/.ssh/id_ed25519"),
            gpu_worker: field(&s.gpu.worker, "desktopgpu"),
            gpu_device: field(&s.gpu.device, "first discrete GPU"),
            xmr_address: field(&s.monero.address, "4... or 8... (95 characters)"),
            xmr_pool: field(&s.monero.pool, "pool.supportxmr.com:443"),
            xmrig_exe: field(&s.monero.xmrig_exe, "automatic (tools/xmrig/xmrig.exe)"),
        }
    }

    fn fill(&self, s: &Settings, cx: &mut Context<Dashboard>) {
        let port = s.oxyhome.port.map(|p| p.to_string()).unwrap_or_default();
        for (input, text) in [
            (&self.miner_exe, s.miner_exe.as_str()),
            (&self.address, s.address.as_str()),
            (&self.pool_url, s.pool_url.as_str()),
            (&self.worker, s.worker.as_str()),
            (&self.ssh_name, s.oxyhome.name.as_str()),
            (&self.ssh_host, s.oxyhome.host.as_str()),
            (&self.ssh_port, port.as_str()),
            (&self.ssh_key, s.oxyhome.key_file.as_str()),
            (&self.gpu_worker, s.gpu.worker.as_str()),
            (&self.gpu_device, s.gpu.device.as_str()),
            (&self.xmr_address, s.monero.address.as_str()),
            (&self.xmr_pool, s.monero.pool.as_str()),
            (&self.xmrig_exe, s.monero.xmrig_exe.as_str()),
        ] {
            input.update(cx, |i, cx| i.set_text(text, cx));
        }
    }
}

impl Dashboard {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings_path = settings::default_path();
        let (settings, message) = match settings::load_from(&settings_path) {
            Ok(s) => {
                // Write the file on first run so it can be edited.
                let msg = if !settings_path.exists() {
                    settings::save_to(&settings_path, &s).err()
                } else {
                    None
                };
                (s, msg)
            }
            Err(e) => (Settings::default(), Some(format!("{e} (using defaults)"))),
        };
        let form = Form::new(&settings, cx);
        let book = logbook::shared();
        let mut this = Self {
            form,
            settings,
            settings_path,
            miner: Miner::with_log(book.clone()),
            local: Vec::new(),
            gpu_miner: Miner::with_log(book.clone()),
            gpu: None,
            idle_secs: 0,
            idle_cpu: false,
            idle_gpu: false,
            idle_tried: false,
            idle_awake: false,
            pool: None,
            network: None,
            xmr_net: None,
            node_sample: None,
            node_rate: None,
            cpu_coin: Coin::Bitcoin,
            oxyhome: None,
            oxyhome_busy: false,
            oxyhome_due: Instant::now(),
            network_due: Instant::now(),
            message,
            logbook: book,
            view: View::Dashboard,
            log_filter: None,
            last_server_error: None,
            hwnd: None,
            xmr_pool: None,
            started_unix: crate::parse::now_unix(),
            metrics_status: None,
            metrics_busy: false,
        };
        if this.settings.start_mining_on_launch {
            this.start_mining();
        }
        if this.settings.gpu.start_on_launch {
            this.start_gpu();
        }
        Self::spawn_local_loop(cx);
        Self::spawn_slow_loop(cx);
        Self::spawn_idle_loop(cx);
        Self::spawn_metrics_loop(cx);
        this
    }

    /// Pushes the metrics every interval while enabled (Settings, Grafana).
    fn spawn_metrics_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let Ok((job, secs)) = this.update(cx, |d, _| {
                    let m = &d.settings.metrics;
                    let job = if m.ready() && !d.metrics_busy {
                        d.metrics_busy = true;
                        Some((m.clone(), d.metrics_text()))
                    } else {
                        None
                    };
                    (job, m.interval_secs.clamp(5, 300))
                }) else {
                    break;
                };
                if let Some((m, body)) = job {
                    let res = cx
                        .background_executor()
                        .spawn(async move { crate::metrics::push(&m, &body) })
                        .await;
                    let _ = this.update(cx, |d, cx| {
                        d.metrics_busy = false;
                        d.record_push(res);
                        cx.notify();
                    });
                }
                cx.background_executor().timer(Duration::from_secs(secs)).await;
            }
        })
        .detach();
    }

    /// Logs only when the push state changes (works / fails with a new error).
    fn record_push(&mut self, res: Result<(), String>) {
        match res {
            Ok(()) => {
                if !matches!(self.metrics_status, Some(Ok(_))) {
                    self.log("metrics: pushing to the Pushgateway");
                }
                self.metrics_status = Some(Ok(crate::parse::now_unix()));
            }
            Err(e) => {
                if self.metrics_status.as_ref().and_then(|s| s.as_ref().err()) != Some(&e) {
                    self.log(&format!("metrics: push failed: {e}"));
                }
                self.metrics_status = Some(Err(e));
            }
        }
    }

    /// Deletes this PC's metrics on Quit so Grafana shows it offline at once.
    pub fn delete_metrics(&self) {
        let m = &self.settings.metrics;
        if m.ready() {
            let _ = crate::metrics::delete(m);
        }
    }

    pub fn set_metrics_enabled(&mut self, enabled: bool) {
        if !enabled && self.settings.metrics.enabled {
            self.delete_metrics();
            self.metrics_status = None;
        }
        self.settings.metrics.enabled = enabled;
        self.save_settings();
        if enabled && self.settings.metrics.password.is_empty() {
            self.message = Some("Metrics: set metrics.password in settings.json, then Reload settings.".into());
        }
    }

    /// Everything the Grafana dashboard shows from this PC, in Prometheus text format.
    pub fn metrics_text(&self) -> String {
        let mut w = crate::metrics::Writer::new();
        let now = crate::parse::now_unix();
        let b = |x: bool| if x { 1.0 } else { 0.0 };
        w.gauge("lm_app_info", "App version (always 1).", &[("version", env!("CARGO_PKG_VERSION"))], 1.0);
        w.gauge("lm_last_push_timestamp_seconds", "When this push was built (unix s); stale means the app is gone.", &[], now);
        w.gauge("lm_app_start_timestamp_seconds", "When the app started (unix s).", &[], self.started_unix);
        for c in [Coin::Bitcoin, Coin::Monero] {
            w.gauge("lm_cpu_coin_selected", "Coin selected for the CPU miners (1 = selected).", &[("coin", ticker(c))], b(self.settings.coin == c));
        }
        w.gauge("lm_idle_seconds", "Seconds since the last keyboard/mouse input.", &[], self.idle_secs as f64);
        w.gauge("lm_idle_mode_enabled", "Idle mining is switched on.", &[], b(self.settings.idle.enabled));
        w.gauge("lm_idle_mining", "Miners started by idle mode are running.", &[], b(self.idle_mining()));
        w.gauge("lm_idle_minutes", "Idle minutes before idle mining starts.", &[], self.settings.idle.minutes as f64);

        let cpu_coin = ticker(self.cpu_coin);
        let cpu_miner = if self.cpu_coin == Coin::Monero { "xmrig" } else { "cpuminer" };
        w.gauge("lm_component_up", "Component running (1) or not (0).", &[("component", "cpu_miner"), ("coin", cpu_coin)], b(self.miner.is_running()));
        w.gauge("lm_component_up", "Component running (1) or not (0).", &[("component", "gpu_miner"), ("coin", "btc")], b(self.gpu_miner.is_running()));
        w.gauge("lm_component_processes", "Miner processes running.", &[("component", "cpu_miner")], self.local.len() as f64);
        for v in &self.local {
            write_process(&mut w, cpu_coin, cpu_miner, v);
        }
        if let Some(g) = &self.gpu {
            write_process(&mut w, "btc", "gpu", g);
        }

        // ckpool solo (Bitcoin): whole address and per worker.
        let ok = matches!(&self.pool, Some(Ok(_)));
        w.gauge("lm_source_up", "Last fetch from this data source worked.", &[("source", "ckpool")], b(ok));
        if let Some(Ok(Some(u))) = &self.pool {
            let p = [("coin", "btc"), ("pool", "ckpool")];
            w.gauge_opt("lm_pool_hashrate_hps", "Hashrate the pool sees, H/s.", &[p[0], p[1], ("worker", "all"), ("window", "1m")], u.hashrate_1m);
            w.gauge_opt("lm_pool_hashrate_hps", "Hashrate the pool sees, H/s.", &[p[0], p[1], ("worker", "all"), ("window", "1h")], u.hashrate_1hr);
            w.gauge_opt("lm_pool_shares", "Shares the pool counted (ckpool resets this on its side).", &[p[0], p[1], ("worker", "all")], u.shares.map(|x| x as f64));
            w.gauge_opt("lm_pool_best_share_difficulty", "Best share difficulty at the pool.", &[p[0], p[1], ("worker", "all"), ("scope", "round")], u.best_share);
            w.gauge_opt("lm_pool_best_share_difficulty", "Best share difficulty at the pool.", &[p[0], p[1], ("worker", "all"), ("scope", "ever")], u.best_ever);
            w.gauge_opt("lm_pool_last_share_timestamp_seconds", "Time of the last share the pool got (unix s).", &[p[0], p[1], ("worker", "all")], u.last_share_unix.map(|t| t as f64));
            for wk in &u.workers {
                let name = wk.name.rsplit('.').next().unwrap_or(&wk.name);
                let l = |extra: (&'static str, &'static str)| [p[0], p[1], ("worker", name), extra];
                w.gauge_opt("lm_pool_hashrate_hps", "Hashrate the pool sees, H/s.", &l(("window", "1m")), wk.hashrate_1m);
                w.gauge_opt("lm_pool_hashrate_hps", "Hashrate the pool sees, H/s.", &l(("window", "1h")), wk.hashrate_1hr);
                w.gauge_opt("lm_pool_best_share_difficulty", "Best share difficulty at the pool.", &l(("scope", "round")), wk.best_share);
                w.gauge_opt("lm_pool_best_share_difficulty", "Best share difficulty at the pool.", &l(("scope", "ever")), wk.best_ever);
                w.gauge_opt("lm_pool_shares", "Shares the pool counted (ckpool resets this on its side).", &[p[0], p[1], ("worker", name)], wk.shares.map(|x| x as f64));
                w.gauge_opt("lm_pool_last_share_timestamp_seconds", "Time of the last share the pool got (unix s).", &[p[0], p[1], ("worker", name)], wk.last_share_unix.map(|t| t as f64));
            }
        }

        // SupportXMR (Monero).
        if !self.settings.monero.solo && self.settings.monero.pool.contains("supportxmr.com") {
            w.gauge("lm_source_up", "Last fetch from this data source worked.", &[("source", "supportxmr")], b(matches!(&self.xmr_pool, Some(Ok(_)))));
        }
        if let Some(Ok(Some(x))) = &self.xmr_pool {
            let p = [("coin", "xmr"), ("pool", "supportxmr")];
            w.gauge("lm_pool_hashrate_hps", "Hashrate the pool sees, H/s.", &[p[0], p[1], ("worker", "all"), ("window", "pool")], x.hashrate);
            w.gauge("lm_pool_shares", "Shares the pool counted (ckpool resets this on its side).", &[p[0], p[1], ("worker", "all")], x.valid_shares as f64);
            w.gauge("lm_pool_invalid_shares", "Invalid shares at the pool.", &[p[0], p[1]], x.invalid_shares as f64);
            w.gauge("lm_pool_total_hashes", "Total hashes the pool credited.", &[p[0], p[1]], x.total_hashes);
            w.gauge_opt("lm_pool_last_share_timestamp_seconds", "Time of the last share the pool got (unix s).", &[p[0], p[1], ("worker", "all")], x.last_share_unix.map(|t| t as f64));
            w.gauge("lm_pool_balance_coins", "Pool balance in coins.", &[p[0], p[1], ("state", "due")], x.due_xmr);
            w.gauge("lm_pool_balance_coins", "Pool balance in coins.", &[p[0], p[1], ("state", "paid")], x.paid_xmr);
        }

        // Bitcoin network (mempool.space).
        w.gauge("lm_source_up", "Last fetch from this data source worked.", &[("source", "mempool")], b(matches!(&self.network, Some(Ok(_)))));
        let net = self.network_numbers();
        let btc = [("coin", "btc")];
        w.gauge_opt("lm_price_eur", "Coin price in EUR.", &btc, net.eur_per_btc);
        w.gauge_opt("lm_network_difficulty", "Network difficulty.", &btc, net.difficulty);
        w.gauge_opt("lm_network_hashrate_hps", "Network hashrate, H/s.", &btc, net.hashrate);
        w.gauge_opt("lm_avg_block_fees_coins", "Average fees per block over the last day.", &btc, net.avg_fees_sats.map(|s| s / 1e8));
        if let (Some(fees), Some(eur)) = (net.avg_fees_sats, net.eur_per_btc) {
            w.gauge("lm_solo_prize_eur", "What a solo block pays after the pool fee, EUR.", &btc, crate::odds::prize_eur(fees, eur));
        }
        w.gauge("lm_pool_fee_ratio", "Pool fee as a fraction.", &[("coin", "btc"), ("pool", "ckpool")], crate::odds::POOL_FEE);
        if let Some(t) = &net.tip {
            w.gauge("lm_block_height", "Latest block height.", &btc, t.height as f64);
            w.gauge("lm_block_timestamp_seconds", "Latest block time (unix s).", &btc, t.timestamp as f64);
            w.gauge("lm_block_reward_coins", "Latest block reward (subsidy + fees), coins.", &btc, t.reward_sats as f64 / 1e8);
            w.gauge("lm_block_fees_coins", "Latest block fees, coins.", &btc, t.fees_sats as f64 / 1e8);
            w.gauge("lm_block_tx_count", "Transactions in the latest block.", &btc, t.tx_count as f64);
            w.gauge("lm_block_size_bytes", "Latest block size.", &btc, t.size as f64);
            let h = t.height.to_string();
            w.gauge("lm_block_info", "Latest block, labels carry the details (always 1).", &[("coin", "btc"), ("height", &h), ("pool", &t.pool)], 1.0);
        }

        // Monero network (moneroblocks.info, CoinGecko).
        w.gauge("lm_source_up", "Last fetch from this data source worked.", &[("source", "moneroblocks")], b(matches!(&self.xmr_net, Some(Ok(_)))));
        if let Some(Ok(x)) = &self.xmr_net {
            let xmr = [("coin", "xmr")];
            w.gauge_opt("lm_price_eur", "Coin price in EUR.", &xmr, x.eur_per_xmr);
            w.gauge_opt("lm_network_difficulty", "Network difficulty.", &xmr, x.difficulty);
            w.gauge_opt("lm_network_hashrate_hps", "Network hashrate, H/s.", &xmr, x.hashrate);
            w.gauge_opt("lm_block_height", "Latest block height.", &xmr, x.height.map(|h| h as f64));
            w.gauge_opt("lm_block_timestamp_seconds", "Latest block time (unix s).", &xmr, x.last_timestamp.map(|t| t as f64));
            w.gauge_opt("lm_block_reward_coins", "Latest block reward (subsidy + fees), coins.", &xmr, x.reward_xmr);
        }
        w.gauge("lm_source_up", "Last fetch from this data source worked.", &[("source", "oxyhome_ssh")], b(matches!(&self.oxyhome, Some(Ok(_)))));
        w.finish()
    }

    /// Idle mode, every 2 s on the main thread (keep_awake is per thread).
    fn spawn_idle_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                if this
                    .update(cx, |d, cx| {
                        d.idle_tick();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
        .detach();
    }

    pub fn idle_tick(&mut self) {
        self.idle_secs = win::idle_seconds();
        let s = self.settings.idle.clone();
        let active = self.idle_cpu || self.idle_gpu || self.idle_tried;
        match settings::idle_action(&s, self.idle_secs, active) {
            IdleAction::Start if !self.idle_tried => {
                self.idle_tried = true;
                self.log(&format!("idle for {} s: idle mode starting the selected miners", self.idle_secs));
                if s.cpu && !self.miner.is_running() {
                    self.start_mining();
                    self.idle_cpu = self.miner.is_running();
                }
                if s.gpu && !self.gpu_miner.is_running() {
                    self.start_gpu();
                    self.idle_gpu = self.gpu_miner.is_running();
                }
            }
            IdleAction::Stop => {
                if self.idle_cpu || self.idle_gpu {
                    self.log("input detected: idle mode stopping the miners it started");
                }
                if self.idle_cpu {
                    self.stop_mining();
                }
                if self.idle_gpu {
                    self.stop_gpu();
                }
                self.idle_tried = false;
            }
            _ => {}
        }
        // Stay awake only while something idle mode started is still running.
        let want_awake = s.enabled
            && s.keep_awake
            && ((self.idle_cpu && self.miner.is_running())
                || (self.idle_gpu && self.gpu_miner.is_running()));
        if want_awake != self.idle_awake {
            win::keep_awake(want_awake);
            self.idle_awake = want_awake;
        }
    }

    /// Adds an app event to the Logs tab.
    pub fn log(&self, text: &str) {
        if let Ok(mut b) = self.logbook.lock() {
            b.push(Source::App, text);
        }
    }

    /// New journal lines go to the log; SSH errors are logged once per change.
    fn record_probe(&mut self, res: &Result<OxyhomeStats, String>) {
        match res {
            Ok(s) => {
                if let Ok(mut b) = self.logbook.lock() {
                    b.push_server(&s.journal);
                }
                // Node sync speed between probes, smoothed, for the time-left estimate.
                if let Some(n) = &s.node {
                    let now = Instant::now();
                    if let Some((t, h)) = self.node_sample {
                        let secs = now.duration_since(t).as_secs_f64();
                        if secs > 5.0 && n.height >= h {
                            let rate = (n.height - h) as f64 / secs;
                            self.node_rate = Some(match self.node_rate {
                                Some(old) => old * 0.7 + rate * 0.3,
                                None => rate,
                            });
                        }
                    }
                    self.node_sample = Some((now, n.height));
                }
                if self.last_server_error.take().is_some() {
                    self.log("oxyhome: SSH connection works again");
                }
            }
            Err(e) => {
                if self.last_server_error.as_deref() != Some(e.as_str()) {
                    self.log(&format!("oxyhome: {e}"));
                    self.last_server_error = Some(e.clone());
                }
            }
        }
    }

    pub fn set_idle_minutes(&mut self, minutes: u32) {
        self.settings.idle.minutes = minutes.clamp(1, 24 * 60);
        self.save_settings();
    }

    /// True while idle mode has miners running.
    pub fn idle_mining(&self) -> bool {
        (self.idle_cpu && self.miner.is_running()) || (self.idle_gpu && self.gpu_miner.is_running())
    }

    /// This PC's CPU miners, in their own coin's unit (see cpu_coin).
    pub fn cpu_hashrate(&self) -> f64 {
        self.local.iter().filter_map(|i| i.hashrate).sum()
    }

    pub fn gpu_hashrate(&self) -> f64 {
        self.gpu.as_ref().and_then(|g| g.hashrate).unwrap_or(0.0)
    }

    /// This PC on Bitcoin: the GPU, plus the CPU miners when they mine Bitcoin.
    pub fn total_hashrate(&self) -> f64 {
        let cpu = if self.cpu_coin == Coin::Bitcoin { self.cpu_hashrate() } else { 0.0 };
        cpu + self.gpu_hashrate()
    }

    /// This PC on Monero (H/s of RandomX): the CPU miners when they mine Monero.
    pub fn xmr_local_hashrate(&self) -> f64 {
        if self.cpu_coin == Coin::Monero { self.cpu_hashrate() } else { 0.0 }
    }

    /// What the home server is set to mine, from its last probe.
    pub fn oxyhome_coin(&self) -> Coin {
        match &self.oxyhome {
            Some(Ok(s)) if s.unit == "xmrig" => Coin::Monero,
            Some(Ok(_)) => Coin::Bitcoin,
            _ => self.settings.coin,
        }
    }

    fn oxyhome_rate(&self, coin: Coin) -> Option<f64> {
        match &self.oxyhome {
            Some(Ok(s)) if s.service_active && self.oxyhome_coin() == coin => s.log.hashrate,
            _ => None,
        }
    }

    /// Home server's Bitcoin hashrate (None while it mines Monero).
    pub fn oxyhome_hashrate(&self) -> Option<f64> {
        self.oxyhome_rate(Coin::Bitcoin)
    }

    /// Home server's Monero hashrate (None while it mines Bitcoin).
    pub fn oxyhome_xmr_hashrate(&self) -> Option<f64> {
        self.oxyhome_rate(Coin::Monero)
    }

    /// Switches the CPU miners here and on the home server to `coin`. A
    /// running CPU miner restarts on the new coin; the GPU stays on Bitcoin.
    pub fn set_coin(&mut self, coin: Coin, cx: &mut Context<Self>) {
        if coin == Coin::Monero && !crate::settings::is_monero_address(self.settings.monero.address.trim()) {
            self.message = Some("Enter your Monero address in Settings and Save before switching to Monero.".into());
            return;
        }
        let changed = self.settings.coin != coin;
        self.settings.coin = coin;
        self.save_settings();
        if changed {
            self.log(&format!("switching CPU mining to {}", coin.label()));
            if self.miner.is_running() {
                let was_idle = self.idle_cpu;
                self.stop_mining();
                self.start_mining();
                self.idle_cpu = was_idle && self.miner.is_running();
            }
        }
        // The server: switch if it is not on this coin yet (also retries).
        if self.oxyhome_coin() != coin || changed {
            self.oxyhome_switch(coin, cx);
        }
    }

    fn oxyhome_switch(&mut self, coin: Coin, cx: &mut Context<Self>) {
        if self.oxyhome_busy {
            return;
        }
        self.oxyhome_busy = true;
        let target = self.settings.oxyhome.clone();
        let monero = self.settings.monero.clone();
        self.log(&format!("oxyhome: switching to {} ({})", coin.label(), coin.server_unit()));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move { remote::switch_coin(&target, coin, &monero) })
                .await;
            let _ = this.update(cx, |d, cx| {
                d.oxyhome_busy = false;
                match result {
                    Ok(()) => d.log(&format!("oxyhome: now mining {}", coin.label())),
                    Err(e) => {
                        d.log(&format!("oxyhome: switch to {} failed: {e}", coin.label()));
                        d.message = Some(format!("oxyhome: {e}"));
                    }
                }
                d.oxyhome_due = Instant::now();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Network difficulty from mempool.space, falling back to the miner log.
    pub fn difficulty(&self) -> Option<f64> {
        if let Some(Ok(n)) = &self.network {
            if n.difficulty.is_some() {
                return n.difficulty;
            }
        }
        self.local.iter().find_map(|i| i.netdiff)
    }

    pub fn network_numbers(&self) -> Network {
        let mut n = match &self.network {
            Some(Ok(n)) => n.clone(),
            _ => Network::default(),
        };
        n.difficulty = self.difficulty();
        n
    }

    /// Starts the CPU miners on the selected coin: cpuminer instances for
    /// Bitcoin, one XMRig process for Monero.
    pub fn start_mining(&mut self) {
        let coin = self.settings.coin;
        let result = match coin {
            Coin::Bitcoin => self.miner.start(&self.settings),
            Coin::Monero => self.miner.start_xmr(&self.settings),
        };
        match result {
            Ok(()) => {
                self.cpu_coin = coin;
                self.message = None;
            }
            Err(e) => {
                self.miner.stop();
                self.log(&format!("CPU mining ({}) failed to start: {e}", coin.label()));
                self.message = Some(e);
            }
        }
    }

    pub fn stop_mining(&mut self) {
        self.miner.stop();
        self.local.clear();
        self.idle_cpu = false;
    }

    pub fn toggle_mining(&mut self) {
        if self.miner.is_running() {
            self.stop_mining();
        } else {
            self.start_mining();
        }
    }

    pub fn start_gpu(&mut self) {
        match self.gpu_miner.start_gpu(&self.settings) {
            Ok(()) => self.message = None,
            Err(e) => {
                self.gpu_miner.stop();
                self.log(&format!("GPU mining failed to start: {e}"));
                self.message = Some(e);
            }
        }
    }

    pub fn stop_gpu(&mut self) {
        self.gpu_miner.stop();
        self.gpu = None;
        self.idle_gpu = false;
    }

    pub fn toggle_gpu(&mut self) {
        if self.gpu_miner.is_running() {
            self.stop_gpu();
        } else {
            self.start_gpu();
        }
    }

    /// Stops everything this app started on this PC (never the home server).
    /// Quick start: the CPU miners and the GPU miner, with the saved
    /// settings. Starts only what is not already running.
    pub fn quick_start(&mut self) {
        if let Some(hint) = self.settings.quick_start_hint() {
            self.message = Some(hint.to_string());
            return;
        }
        if !self.miner.is_running() {
            self.start_mining();
        }
        let cpu_error = self.message.take();
        if !self.gpu_miner.is_running() {
            self.start_gpu();
        }
        // Keep both errors visible if both failed.
        self.message = match (cpu_error, self.message.take()) {
            (Some(c), Some(g)) => Some(format!("CPU: {c}  GPU: {g}")),
            (Some(c), None) => Some(format!("CPU: {c}")),
            (None, Some(g)) => Some(format!("GPU: {g}")),
            (None, None) => None,
        };
    }

    pub fn stop_local_miners(&mut self) {
        self.stop_mining();
        self.stop_gpu();
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = settings::save_to(&self.settings_path, &self.settings) {
            self.message = Some(e);
        }
    }

    /// Reads the miner text fields into settings and saves them.
    pub fn apply_miner_form(&mut self, cx: &mut Context<Self>) {
        let f = &self.form;
        let mut s = self.settings.clone();
        s.miner_exe = f.miner_exe.read(cx).text().trim().to_string();
        s.address = f.address.read(cx).text().trim().to_string();
        s.pool_url = f.pool_url.read(cx).text().trim().to_string();
        s.worker = f.worker.read(cx).text().trim().to_string();
        let problems = s.validate();
        if !problems.is_empty() {
            self.message = Some(problems.join("; "));
            return;
        }
        self.settings = s;
        self.save_settings();
        self.network_due = Instant::now();
        self.message = Some(if self.miner.is_running() {
            "Saved. Stop and start mining to use the new miner settings.".into()
        } else {
            "Saved.".into()
        });
    }

    /// Reads the Monero text fields into settings and saves them.
    pub fn apply_monero_form(&mut self, cx: &mut Context<Self>) {
        let address = self.form.xmr_address.read(cx).text().trim().to_string();
        let pool = self.form.xmr_pool.read(cx).text().trim().to_string();
        let exe = self.form.xmrig_exe.read(cx).text().trim().to_string();
        if !address.is_empty() && !settings::is_monero_address(&address) {
            self.message = Some("That is not a valid Monero address (95 or 106 characters, starts with 4 or 8).".into());
            return;
        }
        if self.settings.monero.solo && !address.is_empty() && !settings::is_standard_monero_address(&address) {
            self.message = Some("Solo mining needs your wallet's main address (starts with 4), not a subaddress.".into());
            return;
        }
        if let Some(hint) = settings::monero_node_port_hint(&pool) {
            self.message = Some(hint.to_string());
            return;
        }
        if !settings::is_pool_host_port(&pool) {
            self.message = Some("Monero pool must be host:port, for example pool.supportxmr.com:443.".into());
            return;
        }
        let m = &mut self.settings.monero;
        let changed = m.address != address || m.pool != pool;
        m.address = address;
        m.pool = pool;
        m.xmrig_exe = exe;
        self.save_settings();
        self.message = Some(if changed && self.settings.coin == Coin::Monero {
            "Saved. Switch to Bitcoin and back to Monero to apply it on running miners and oxyhome.".into()
        } else {
            "Saved.".into()
        });
    }

    /// Reads the GPU text fields into settings and saves them.
    pub fn apply_gpu_form(&mut self, cx: &mut Context<Self>) {
        let worker = self.form.gpu_worker.read(cx).text().trim().to_string();
        if worker.is_empty() || worker.contains(char::is_whitespace) || worker.contains('.') {
            self.message = Some("GPU worker name must be one word without dots.".into());
            return;
        }
        self.settings.gpu.worker = worker;
        self.settings.gpu.device = self.form.gpu_device.read(cx).text().trim().to_string();
        self.save_settings();
        self.message = Some(if self.gpu_miner.is_running() {
            "Saved. Stop and start GPU mining to use the new settings.".into()
        } else {
            "Saved.".into()
        });
    }

    /// Reads the SSH form, saves it and probes the machine right away.
    pub fn apply_ssh_form(&mut self, cx: &mut Context<Self>) {
        let f = &self.form;
        let port = match settings::parse_port(&f.ssh_port.read(cx).text()) {
            Ok(p) => p,
            Err(e) => {
                self.message = Some(e);
                return;
            }
        };
        let host = f.ssh_host.read(cx).text().trim().to_string();
        if host.is_empty() {
            self.message = Some("SSH host is empty.".into());
            return;
        }
        self.settings.oxyhome.name = f.ssh_name.read(cx).text().trim().to_string();
        self.settings.oxyhome.host = host;
        self.settings.oxyhome.port = port;
        self.settings.oxyhome.key_file = f.ssh_key.read(cx).text().trim().to_string();
        self.save_settings();
        self.oxyhome = None;
        self.oxyhome_due = Instant::now();
        self.message = None;
    }

    pub fn reload_settings(&mut self, cx: &mut Context<Self>) {
        match settings::load_from(&self.settings_path) {
            Ok(s) => {
                self.form.fill(&s, cx);
                self.settings = s;
                self.message = Some(if self.miner.is_running() {
                    "Settings reloaded; restart mining to apply miner changes.".into()
                } else {
                    "Settings reloaded.".into()
                });
                self.oxyhome_due = Instant::now();
                self.network_due = Instant::now();
            }
            Err(e) => self.message = Some(e),
        }
    }

    pub fn set_start_at_login(&mut self, enabled: bool) {
        match win::set_start_at_login(enabled) {
            Ok(()) => {
                self.settings.start_at_login = enabled;
                self.save_settings();
            }
            Err(e) => self.message = Some(e),
        }
    }

    pub fn show_window(&self) {
        if let Some(h) = self.hwnd {
            win::show_window(h);
        }
    }

    /// False only once the server answered that cpuminer.service does not exist.
    pub fn oxyhome_unit_present(&self) -> bool {
        !matches!(&self.oxyhome, Some(Ok(s)) if !s.unit_found)
    }

    /// Runs systemctl start|stop|restart cpuminer on oxyhome in the background.
    pub fn oxyhome_control(&mut self, action: ServiceAction, cx: &mut Context<Self>) {
        if self.oxyhome_busy {
            return;
        }
        if !self.oxyhome_unit_present() {
            self.message = Some(format!(
                "{} has no cpuminer.service; nothing to start or stop.",
                self.settings.oxyhome.host
            ));
            return;
        }
        self.oxyhome_busy = true;
        let target = self.settings.oxyhome.clone();
        let coin = self.oxyhome_coin();
        let unit = coin.server_unit();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let what = format!("{:?}", action).to_lowercase();
            let result = cx
                .background_executor()
                .spawn(async move { remote::control(&target, action, coin) })
                .await;
            let _ = this.update(cx, |d, cx| {
                d.oxyhome_busy = false;
                match result {
                    Ok(()) => d.log(&format!("oxyhome: systemctl {what} {unit} done")),
                    Err(e) => {
                        d.log(&format!("oxyhome: systemctl {what} {unit} failed: {e}"));
                        d.message = Some(format!("oxyhome: {e}"));
                    }
                }
                d.oxyhome_due = Instant::now();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Local miners: API and log every refresh interval.
    fn spawn_local_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let Ok((handles, gpu_handles, secs, exited, gpu_exit)) = this.update(cx, |d, _| {
                    let exited = d.miner.reap_exited();
                    let gpu_exit = d.gpu_miner.reap_exited_errors();
                    (
                        d.miner.handles(),
                        d.gpu_miner.handles(),
                        d.settings.refresh_secs,
                        exited,
                        gpu_exit,
                    )
                }) else {
                    break;
                };
                let (views, gpu_views) = cx
                    .background_executor()
                    .spawn(async move { (collect_local(handles), collect_local(gpu_handles)) })
                    .await;
                let ok = this.update(cx, |d, cx| {
                    if exited > 0 {
                        let m = format!(
                            "{exited} miner instance(s) exited on their own; check the miner path and pool."
                        );
                        d.log(&m);
                        d.message = Some(m);
                    }
                    if let Some(err) = gpu_exit.into_iter().next() {
                        let m = format!("GPU miner exited: {err}");
                        d.log(&m);
                        d.message = Some(m);
                    }
                    d.local = views;
                    d.gpu = gpu_views.into_iter().next();
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(secs.max(2)))
                    .await;
            }
        })
        .detach();
    }

    /// Pool, mempool and oxyhome on their own slower clocks.
    fn spawn_slow_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let Ok((net_job, oxy_job)) = this.update(cx, |d, _| {
                    let now = Instant::now();
                    let net = if now >= d.network_due {
                        d.network_due = now + NETWORK_EVERY;
                        Some((
                            d.settings.address.clone(),
                            // Solo: no pool to ask (empty skips SupportXMR).
                            if d.settings.monero.solo { String::new() } else { d.settings.monero.pool.clone() },
                            d.settings.monero.address.clone(),
                        ))
                    } else {
                        None
                    };
                    let oxy = if now >= d.oxyhome_due && !d.oxyhome_busy {
                        d.oxyhome_due = now + OXYHOME_EVERY;
                        d.oxyhome_busy = true;
                        Some(d.settings.oxyhome.clone())
                    } else {
                        None
                    };
                    (net, oxy)
                }) else {
                    break;
                };
                if let Some((address, xmr_pool, xmr_address)) = net_job {
                    let weak = this.clone();
                    let mut acx = cx.clone();
                    cx.spawn(async move |_| {
                        let (pool, net, xmr, xpool) = acx
                            .background_executor()
                            .spawn(async move {
                                (
                                    network::fetch_pool_user(&address),
                                    network::fetch_network(),
                                    network::fetch_monero(),
                                    network::fetch_xmr_pool(&xmr_pool, &xmr_address),
                                )
                            })
                            .await;
                        let _ = weak.update(&mut acx, |d, cx| {
                            d.pool = Some(pool);
                            d.network = Some(net);
                            d.xmr_net = Some(xmr);
                            d.xmr_pool = Some(xpool);
                            cx.notify();
                        });
                    })
                    .detach();
                }
                if let Some(target) = oxy_job {
                    let weak = this.clone();
                    let mut acx = cx.clone();
                    cx.spawn(async move |_| {
                        let res = acx
                            .background_executor()
                            .spawn(async move { remote::probe(&target) })
                            .await;
                        let _ = weak.update(&mut acx, |d, cx| {
                            d.record_probe(&res);
                            d.oxyhome = Some(res);
                            d.oxyhome_busy = false;
                            cx.notify();
                        });
                    })
                    .detach();
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();
    }
}

fn ticker(c: Coin) -> &'static str {
    match c {
        Coin::Bitcoin => "btc",
        Coin::Monero => "xmr",
    }
}

/// One miner process: hashrate, shares, jobs, best and latest share.
fn write_process(w: &mut crate::metrics::Writer, coin: &str, miner: &str, v: &InstanceView) {
    let proc_ = v.index.to_string();
    let l = [("coin", coin), ("miner", miner), ("proc", proc_.as_str())];
    let opt = |x: Option<f64>| x;
    w.gauge("lm_hashrate_hps", "Measured hashrate, H/s.", &l, v.hashrate.unwrap_or(0.0));
    w.gauge("lm_miner_connected", "Connected to the pool (1) or not (0).", &l, if v.connected { 1.0 } else { 0.0 });
    w.gauge("lm_miner_stalled", "Waiting for work after running out of nonces.", &l, if v.stalled_now { 1.0 } else { 0.0 });
    w.gauge("lm_miner_uptime_seconds", "Seconds since this miner process started.", &l, v.uptime.as_secs_f64());
    let res = |r: &'static str| [l[0], l[1], l[2], ("result", r)];
    w.counter("lm_shares_total", "Shares since the miner started, by pool verdict.", &res("accepted"), v.accepted as f64);
    w.counter("lm_shares_total", "Shares since the miner started, by pool verdict.", &res("rejected"), v.rejected as f64);
    w.counter("lm_jobs_total", "Jobs (new work) received from the pool.", &l, v.jobs as f64);
    w.gauge_opt("lm_job_last_timestamp_seconds", "When the last job arrived (unix s).", &l, opt(v.last_job_unix));
    w.counter("lm_stalls_total", "Times the miner ran out of nonces.", &l, v.stall_count as f64);
    w.counter("lm_stratum_resets_total", "Stratum reconnects.", &l, v.stratum_resets as f64);
    w.counter("lm_blocks_found_total", "Shares at or above network difficulty (blocks).", &l, v.blocks_found as f64);
    w.gauge_opt("lm_share_target_difficulty", "Share difficulty the pool asks for.", &l, v.pool_diff);
    w.gauge_opt("lm_share_last_difficulty", "Difficulty of the latest share (XMRig logs the difficulty the pool credits).", &l, v.last_share_diff);
    w.gauge_opt("lm_share_best_difficulty", "Best share difficulty since the miner started.", &l, v.best_share_diff);
    w.gauge_opt("lm_share_last_timestamp_seconds", "When the latest share was submitted (unix s).", &l, v.last_share_unix);
    w.gauge_opt("lm_job_block_height", "Block height of the current job.", &l, v.block.map(|b| b as f64));
}

/// Merges the API reply (preferred) with the log state for each instance.
fn collect_local(handles: Vec<InstanceHandle>) -> Vec<InstanceView> {
    handles
        .into_iter()
        .map(|h| {
            let log = h.log.lock().map(|l| l.clone()).unwrap_or_default();
            let mut v = InstanceView {
                index: h.index,
                port: h.port,
                hashrate: log.hashrate,
                from_api: false,
                accepted: log.accepted,
                rejected: log.rejected(),
                uptime: h.started.elapsed(),
                stall_count: log.stall_count,
                stalled_now: log.stalled_now,
                api_error: None,
                netdiff: log.netdiff,
                connected: log.connected,
                device: None,
                last_error: log.last_error.clone(),
                stratum_resets: log.stratum_resets,
                pool_diff: log.difficulty,
                block: log.block,
                jobs: log.jobs,
                last_job_unix: log.last_job_unix,
                last_share_diff: log.last_share_diff,
                best_share_diff: log.best_share_diff,
                last_share_unix: log.last_share_unix,
                blocks_found: log.blocks_found,
            };
            match miner::query_stats(&h) {
                Ok(s) => {
                    if s.hashrate > 0.0 {
                        v.hashrate = Some(s.hashrate);
                        v.from_api = true;
                    }
                    v.accepted = v.accepted.max(s.accepted);
                    v.rejected = v.rejected.max(s.rejected);
                    v.device = s.device;
                    if let Some(b) = s.best_share {
                        v.best_share_diff = Some(v.best_share_diff.map_or(b, |x| x.max(b)));
                    }
                    if h.kind == miner::ApiKind::Xmrig && v.pool_diff.is_none() {
                        v.pool_diff = s.difficulty;
                    }
                    // The API's DIFF is the network difficulty.
                    if v.netdiff.is_none() {
                        v.netdiff = s.difficulty.filter(|d| *d > 1e6);
                    }
                }
                Err(e) => v.api_error = Some(e),
            }
            v
        })
        .collect()
}
