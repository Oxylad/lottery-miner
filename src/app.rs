//! Dashboard state, the refresh loops and the actions the UI and tray call.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext, AsyncApp, Context, Entity, WeakEntity};

use crate::input::TextInput;
use crate::miner::{self, InstanceHandle, Miner};
use crate::network::{self, Network, PoolUser};
use crate::remote::{self, OxyhomeStats, ServiceAction};
use crate::settings::{self, Settings};
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
}

pub struct Dashboard {
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub miner: Miner,
    pub local: Vec<InstanceView>,
    /// The GPU miner process (gpu-miner.exe), separate from the CPU instances.
    pub gpu_miner: Miner,
    pub gpu: Option<InstanceView>,
    pub pool: Option<Result<Option<PoolUser>, String>>,
    pub network: Option<Result<Network, String>>,
    pub oxyhome: Option<Result<OxyhomeStats, String>>,
    pub oxyhome_busy: bool,
    pub oxyhome_due: Instant,
    pub network_due: Instant,
    pub message: Option<String>,
    pub hwnd: Option<isize>,
    pub form: Form,
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
        let mut this = Self {
            form,
            settings,
            settings_path,
            miner: Miner::default(),
            local: Vec::new(),
            gpu_miner: Miner::default(),
            gpu: None,
            pool: None,
            network: None,
            oxyhome: None,
            oxyhome_busy: false,
            oxyhome_due: Instant::now(),
            network_due: Instant::now(),
            message,
            hwnd: None,
        };
        if this.settings.start_mining_on_launch {
            this.start_mining();
        }
        if this.settings.gpu.start_on_launch {
            this.start_gpu();
        }
        Self::spawn_local_loop(cx);
        Self::spawn_slow_loop(cx);
        this
    }

    pub fn cpu_hashrate(&self) -> f64 {
        self.local.iter().filter_map(|i| i.hashrate).sum()
    }

    pub fn gpu_hashrate(&self) -> f64 {
        self.gpu.as_ref().and_then(|g| g.hashrate).unwrap_or(0.0)
    }

    /// This PC: CPU instances plus the GPU miner.
    pub fn total_hashrate(&self) -> f64 {
        self.cpu_hashrate() + self.gpu_hashrate()
    }

    pub fn oxyhome_hashrate(&self) -> Option<f64> {
        match &self.oxyhome {
            Some(Ok(s)) if s.service_active => s.log.hashrate,
            _ => None,
        }
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

    pub fn start_mining(&mut self) {
        match self.miner.start(&self.settings) {
            Ok(()) => self.message = None,
            Err(e) => {
                self.miner.stop();
                self.message = Some(e);
            }
        }
    }

    pub fn stop_mining(&mut self) {
        self.miner.stop();
        self.local.clear();
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
                self.message = Some(e);
            }
        }
    }

    pub fn stop_gpu(&mut self) {
        self.gpu_miner.stop();
        self.gpu = None;
    }

    pub fn toggle_gpu(&mut self) {
        if self.gpu_miner.is_running() {
            self.stop_gpu();
        } else {
            self.start_gpu();
        }
    }

    /// Stops everything this app started on this PC (never the home server).
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
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = cx
                .background_executor()
                .spawn(async move { remote::control(&target, action) })
                .await;
            let _ = this.update(cx, |d, cx| {
                d.oxyhome_busy = false;
                if let Err(e) = result {
                    d.message = Some(format!("oxyhome: {e}"));
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
                        d.message = Some(format!(
                            "{exited} miner instance(s) exited on their own; check the miner path and pool."
                        ));
                    }
                    if let Some(err) = gpu_exit.into_iter().next() {
                        d.message = Some(format!("GPU miner exited: {err}"));
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
                        Some(d.settings.address.clone())
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
                if let Some(address) = net_job {
                    let weak = this.clone();
                    let mut acx = cx.clone();
                    cx.spawn(async move |_| {
                        let (pool, net) = acx
                            .background_executor()
                            .spawn(async move {
                                (network::fetch_pool_user(&address), network::fetch_network())
                            })
                            .await;
                        let _ = weak.update(&mut acx, |d, cx| {
                            d.pool = Some(pool);
                            d.network = Some(net);
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
            };
            match miner::query_summary(h.port) {
                Ok(s) => {
                    if s.hashrate > 0.0 {
                        v.hashrate = Some(s.hashrate);
                        v.from_api = true;
                    }
                    v.accepted = v.accepted.max(s.accepted);
                    v.rejected = v.rejected.max(s.rejected);
                    v.device = s.device;
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
