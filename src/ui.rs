//! GPUI rendering of the dashboard and settings.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, Context, Entity, FontWeight, MouseButton, MouseDownEvent, SharedString, Window,
    div, prelude::*, px, relative, rgb,
};

use crate::app::Dashboard;
use crate::input::TextInput;
use crate::network::{self, Network};
use crate::odds;
use crate::remote::ServiceAction;
use crate::settings::MAX_MHS_PER_INSTANCE;

const BG: u32 = 0x1b1d22;
const CARD: u32 = 0x252830;
const BORDER: u32 = 0x343844;
const TEXT: u32 = 0xe6e6e6;
const MUTED: u32 = 0x8a8f9c;
const ACCENT: u32 = 0xf7931a;
const WARN: u32 = 0xe5a50a;
const ERROR: u32 = 0xe5534b;
const GOOD: u32 = 0x57ab5a;
const BUTTON: u32 = 0x34528a;
const BUTTON_OFF: u32 = 0x3a3d47;

fn card(title: &str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(rgb(CARD))
        .border_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .text_color(rgb(ACCENT))
                .font_weight(FontWeight::BOLD)
                .child(title.to_string()),
        )
}

fn row(label: impl Into<SharedString>, value: impl Into<SharedString>) -> AnyElement {
    row_colored(label, value, TEXT)
}

fn row_colored(label: impl Into<SharedString>, value: impl Into<SharedString>, color: u32) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .child(div().flex_none().text_color(rgb(MUTED)).child(label.into()))
        .child(
            div()
                .min_w(px(0.))
                .text_right()
                .text_color(rgb(color))
                .child(value.into()),
        )
        .into_any_element()
}

fn note(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().text_sm().text_color(rgb(color)).child(text.into()).into_any_element()
}

fn button(
    label: impl Into<SharedString>,
    enabled: bool,
    cx: &Context<Dashboard>,
    f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    let el = div()
        .px_3()
        .py_1()
        .rounded_md()
        .text_sm()
        .text_color(rgb(if enabled { TEXT } else { MUTED }))
        .bg(rgb(if enabled { BUTTON } else { BUTTON_OFF }))
        .child(label.into());
    if enabled {
        el.cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    f(this, cx);
                    cx.notify();
                }),
            )
            .into_any_element()
    } else {
        el.into_any_element()
    }
}

fn toggle(
    label: &str,
    on: bool,
    cx: &Context<Dashboard>,
    f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(div().text_color(rgb(MUTED)).child(label.to_string()))
        .child(button(if on { "On" } else { "Off" }, true, cx, f))
        .into_any_element()
}

fn stepper(
    label: String,
    value: String,
    cx: &Context<Dashboard>,
    dec: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
    inc: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(div().text_color(rgb(MUTED)).child(label))
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(button("-", true, cx, dec))
                .child(div().w_8().text_color(rgb(TEXT)).child(value))
                .child(button("+", true, cx, inc)),
        )
        .into_any_element()
}

/// Label, text input and helper line, like a settings form.
fn field(label: &str, input: Entity<TextInput>, help: impl Into<SharedString>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .py_1()
        .child(div().text_sm().font_weight(FontWeight::BOLD).child(label.to_string()))
        .child(input)
        .child(div().text_xs().text_color(rgb(MUTED)).child(help.into()))
        .into_any_element()
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 86_400 {
        format!("{}d {:02}h", s / 86_400, (s % 86_400) / 3600)
    } else if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

fn fmt_ago(unix: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = (now - unix).max(0) as u64;
    format!("{} ago", fmt_duration(Duration::from_secs(diff)))
}

fn fmt_hr(hs: Option<f64>) -> String {
    hs.map(odds::format_hashrate).unwrap_or_else(|| "-".into())
}

fn fmt_eur(v: f64) -> String {
    // Thousands separators, no decimals.
    let n = v.round() as i64;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("EUR {out}")
}

fn odds_rows(label: &str, hs: Option<f64>, net: &Network) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let Some(hs) = hs.filter(|h| *h > 0.0) else {
        rows.push(row(label.to_string(), "not mining"));
        return rows;
    };
    if net.difficulty.is_none() {
        rows.push(row(label.to_string(), "waiting for network difficulty"));
        return rows;
    }
    let o = network::odds_for(hs, net);
    let year = o
        .one_in_per_year
        .map(|n| format!("{} per year", odds::format_one_in(n)))
        .unwrap_or_else(|| "-".into());
    let day = if o.blocks_per_day > 0.0 {
        format!("{} per day", odds::format_one_in(1.0 / o.blocks_per_day))
    } else {
        "-".into()
    };
    rows.push(row(format!("{label} ({})", odds::format_hashrate(hs)), year));
    rows.push(row("", day));
    rows
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.miner.is_running();
        let total = self.total_hashrate();
        let net = self.network_numbers();

        // ---- this PC
        let mut local = card("This PC (cpuminer-opt)");
        local = local.child(
            div()
                .flex()
                .gap_2()
                .child(button("Start CPU mining", !running, cx, |d, _| d.start_mining()))
                .child(button("Stop CPU mining", running, cx, |d, _| d.stop_mining())),
        );
        local = local.child(row_colored(
            "Status",
            if running {
                format!("running, {} instance(s)", self.local.len().max(self.miner.handles().len()))
            } else {
                "stopped".into()
            },
            if running { GOOD } else { MUTED },
        ));
        local = local.child(row("Total hashrate", fmt_hr(Some(total).filter(|t| *t > 0.0))));
        let mut stalls = 0;
        for v in &self.local {
            let speed = fmt_hr(v.hashrate);
            let src = if v.stalled_now {
                " (idle, waiting for work)"
            } else if v.from_api {
                ""
            } else {
                " (log)"
            };
            local = local.child(row(
                format!("#{} port {}", v.index + 1, v.port),
                format!(
                    "{speed}{src}   acc {} / rej {}   up {}",
                    v.accepted,
                    v.rejected,
                    fmt_duration(v.uptime)
                ),
            ));
            if v.hashrate.unwrap_or(0.0) > MAX_MHS_PER_INSTANCE * 1e6 {
                local = local.child(note(
                    format!(
                        "Instance #{} is above {MAX_MHS_PER_INSTANCE:.0} MH/s; it will run out of nonces and idle. Use more instances with fewer threads.",
                        v.index + 1
                    ),
                    WARN,
                ));
            }
            stalls += v.stall_count;
        }
        if stalls > 0 {
            local = local.child(note(
                format!("Stall warning: \"Nonce range exhausted, extranonce not subscribed\" seen {stalls} time(s). Instances idle until the next job."),
                WARN,
            ));
        }
        if running && self.local.iter().all(|v| v.api_error.is_some()) && !self.local.is_empty() {
            local = local.child(note(
                "Stats API not reachable yet; using log lines.",
                MUTED,
            ));
        }

        // ---- home server over SSH
        let busy = self.oxyhome_busy;
        let can_control = !busy && self.oxyhome_unit_present();
        let target = &self.settings.oxyhome;
        let (dot, status): (u32, String) = match &self.oxyhome {
            None => (MUTED, "connecting...".into()),
            Some(Err(_)) => (ERROR, "cannot connect".into()),
            Some(Ok(s)) if !s.unit_found => (
                WARN,
                "connected, but this server has no cpuminer.service".into(),
            ),
            Some(Ok(s)) if s.service_active => (
                GOOD,
                format!("cpuminer active, {}", fmt_hr(s.log.hashrate)),
            ),
            Some(Ok(_)) => (WARN, "connected, cpuminer not active".into()),
        };
        let mut oxy = card("Home server (SSH)");
        oxy = oxy.child(
            div()
                .flex()
                .gap_2()
                .items_start()
                .py_1()
                .child(div().mt_1p5().size_2().rounded_full().bg(rgb(dot)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .child(if target.name.is_empty() { target.host.clone() } else { target.name.clone() }),
                        )
                        .child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                            "{}{}",
                            target.host,
                            target.port.map(|p| format!(":{p}")).unwrap_or_default()
                        )))
                        .child(div().text_sm().text_color(rgb(MUTED)).child(status)),
                ),
        );
        oxy = oxy.child(
            div()
                .flex()
                .gap_2()
                .child(button("Start", can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Start, cx)))
                .child(button("Stop", can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Stop, cx)))
                .child(button("Restart", can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Restart, cx))),
        );
        match &self.oxyhome {
            None => oxy = oxy.child(note("Connecting over SSH...", MUTED)),
            Some(Err(e)) => oxy = oxy.child(note(e.clone(), ERROR)),
            Some(Ok(s)) => {
                oxy = oxy.child(row_colored(
                    "Service",
                    if s.service_active { "active" } else { "not active" },
                    if s.service_active { GOOD } else { WARN },
                ));
                if let Some(since) = s.active_since.as_ref().filter(|v| !v.is_empty()) {
                    oxy = oxy.child(row("Active since", since.clone()));
                }
                if let Some(r) = s.restarts {
                    oxy = oxy.child(row("Restarts", r.to_string()));
                }
                oxy = oxy.child(row("Hashrate", fmt_hr(s.log.hashrate)));
                oxy = oxy.child(row(
                    "Shares",
                    format!("acc {} / rej {}", s.log.accepted, s.log.rejected()),
                ));
                oxy = oxy.child(row(
                    "CPU package",
                    s.temp_c.map(|t| format!("{t:.0} C")).unwrap_or_else(|| "-".into()),
                ));
                oxy = oxy.child(row(
                    "Power (RAPL)",
                    s.watts.map(|w| format!("{w:.0} W")).unwrap_or_else(|| "-".into()),
                ));
            }
        }
        if busy {
            oxy = oxy.child(note("Working...", MUTED));
        }
        let f = &self.form;
        oxy = oxy
            .child(div().h_px().my_2().bg(rgb(BORDER)))
            .child(field("Name", f.ssh_name.clone(), "A name for this machine."))
            .child(field(
                "SSH host",
                f.ssh_host.clone(),
                "user@myserver.com, or a Host from ~/.ssh/config.",
            ))
            .child(field(
                "SSH port",
                f.ssh_port.clone(),
                "Leave empty for the default (22) or what your SSH configuration says.",
            ))
            .child(field(
                "SSH key (optional)",
                f.ssh_key.clone(),
                "Path to a private key. Leave empty to use your SSH configuration or agent.",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(button("Save and connect", !busy, cx, |d, cx| d.apply_ssh_form(cx))),
            )
            .child(note(
                "Control is limited to: sudo -n systemctl start|stop|restart cpuminer.",
                MUTED,
            ));

        // ---- odds
        let oxy_hr = self.oxyhome_hashrate();
        let combined = Some(total + oxy_hr.unwrap_or(0.0));
        let mut odds_card = card("Odds of finding a block");
        for r in odds_rows("This PC", Some(total), &net) {
            odds_card = odds_card.child(r);
        }
        let server_name = if self.settings.oxyhome.name.is_empty() {
            self.settings.oxyhome.host.clone()
        } else {
            self.settings.oxyhome.name.clone()
        };
        for r in odds_rows(&server_name, oxy_hr, &net) {
            odds_card = odds_card.child(r);
        }
        for r in odds_rows("Combined", combined, &net) {
            odds_card = odds_card.child(r);
        }
        if let Some(d) = net.difficulty {
            odds_card = odds_card.child(row("Network difficulty", format!("{d:.4e}")));
        }
        if let (Some(fees), Some(eur)) = (net.avg_fees_sats, net.eur_per_btc) {
            let btc = (odds::SUBSIDY_BTC + fees / 1e8) * (1.0 - odds::POOL_FEE);
            odds_card = odds_card.child(row(
                "Prize after 2% pool fee",
                format!("{btc:.3} BTC = {}", fmt_eur(odds::prize_eur(fees, eur))),
            ));
        }
        if let Some(t) = odds::seconds_per_pool_share(total) {
            odds_card = odds_card.child(row(
                "Expected pool share (this PC)",
                format!("every {}", fmt_duration(Duration::from_secs_f64(t))),
            ));
        }
        if let Some(Err(e)) = &self.network {
            odds_card = odds_card.child(note(format!("mempool.space: {e}"), ERROR));
        }

        // ---- pool
        let mut pool = card("Pool (eusolo.ckpool.org)");
        let pool_url = network::pool_user_url(&self.settings.address);
        pool = pool.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w(px(0.))
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(pool_url.clone()),
                )
                .child(button("Open on ckpool", true, cx, move |_, cx| {
                    cx.open_url(&pool_url);
                })),
        );
        match &self.pool {
            None => pool = pool.child(note("Loading...", MUTED)),
            Some(Err(e)) => pool = pool.child(note(e.clone(), ERROR)),
            Some(Ok(None)) => {
                pool = pool.child(note(
                    "No accepted share yet for this address (404 is normal until the first share).",
                    MUTED,
                ))
            }
            Some(Ok(Some(u))) => {
                if let Some(b) = u.best_share {
                    let short = net
                        .difficulty
                        .and_then(|d| odds::times_short(d, b))
                        .map(|n| format!("  ({n:.3e} times short)"))
                        .unwrap_or_default();
                    pool = pool.child(row("Best share", format!("{b:.0}{short}")));
                }
                if let Some(b) = u.best_ever {
                    pool = pool.child(row("Best ever", format!("{b:.0}")));
                }
                if let Some(t) = u.last_share_unix {
                    pool = pool.child(row("Last share", fmt_ago(t)));
                }
                if let Some(s) = u.shares {
                    pool = pool.child(row("Shares", s.to_string()));
                }
                pool = pool.child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(format!(
                            "Share-based estimates: 1m {}, 1h {}",
                            fmt_hr(u.hashrate_1m),
                            fmt_hr(u.hashrate_1hr)
                        )),
                );
                for w in &u.workers {
                    let name = w.name.rsplit('.').next().unwrap_or(&w.name).to_string();
                    pool = pool.child(
                        div().text_sm().text_color(rgb(MUTED)).child(format!(
                            "  {name}: 1m {}, last share {}",
                            fmt_hr(w.hashrate_1m),
                            w.last_share_unix.map(fmt_ago).unwrap_or_else(|| "-".into())
                        )),
                    );
                }
            }
        }

        // ---- settings
        let s = &self.settings;
        let per_instance_estimate = 290.0 / 16.0 * s.threads_per_instance as f64;
        let mut set = card("Settings");
        set = set
            .child(field("Miner executable", f.miner_exe.clone(), "cpuminer-opt build for this CPU."))
            .child(field("Bitcoin address", f.address.clone(), "Payout address; also the pool user name."))
            .child(field("Pool URL", f.pool_url.clone(), "ckpool solo, EU region."))
            .child(field(
                "Worker name",
                f.worker.clone(),
                format!("Pool user is {}.<worker>.", short_addr(&s.address)),
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(button("Save", true, cx, |d, cx| d.apply_miner_form(cx))),
            );
        set = set.child(stepper(
            "Instances".into(),
            s.instances.to_string(),
            cx,
            |d, _| {
                d.settings.instances = d.settings.instances.saturating_sub(1).max(1);
                d.save_settings();
            },
            |d, _| {
                if (d.settings.instances + 1) * d.settings.threads_per_instance <= 16 {
                    d.settings.instances += 1;
                    d.save_settings();
                }
            },
        ));
        set = set.child(stepper(
            "Threads per instance".into(),
            s.threads_per_instance.to_string(),
            cx,
            |d, _| {
                d.settings.threads_per_instance = d.settings.threads_per_instance.saturating_sub(1).max(1);
                d.save_settings();
            },
            |d, _| {
                if d.settings.instances * (d.settings.threads_per_instance + 1) <= 16 {
                    d.settings.threads_per_instance += 1;
                    d.save_settings();
                }
            },
        ));
        set = set.child(note(
            format!(
                "Rule: keep each instance under {MAX_MHS_PER_INSTANCE:.0} MH/s. About {per_instance_estimate:.0} MH/s per instance at {} threads on this CPU.",
                s.threads_per_instance
            ),
            if per_instance_estimate > MAX_MHS_PER_INSTANCE { WARN } else { MUTED },
        ));
        set = set.child(row("API ports", format!("{}-{}", s.api_base_port, s.api_base_port as u32 + s.instances.saturating_sub(1))));
        set = set.child(stepper(
            "Refresh (s)".into(),
            s.refresh_secs.to_string(),
            cx,
            |d, _| {
                d.settings.refresh_secs = d.settings.refresh_secs.saturating_sub(1).max(2);
                d.save_settings();
            },
            |d, _| {
                d.settings.refresh_secs = (d.settings.refresh_secs + 1).min(300);
                d.save_settings();
            },
        ));
        set = set.child(toggle("Start mining when the app starts", s.start_mining_on_launch, cx, |d, _| {
            d.settings.start_mining_on_launch = !d.settings.start_mining_on_launch;
            d.save_settings();
        }));
        set = set.child(toggle("Start app at login", s.start_at_login, cx, |d, _| {
            let v = !d.settings.start_at_login;
            d.set_start_at_login(v);
        }));
        set = set.child(toggle("Stop this PC's miners on Quit", s.stop_miners_on_quit, cx, |d, _| {
            d.settings.stop_miners_on_quit = !d.settings.stop_miners_on_quit;
            d.save_settings();
        }));
        set = set.child(note(
            format!(
                "{} is never stopped when this app quits or crashes; it only changes when you press its Start, Stop or Restart.",
                server_name
            ),
            MUTED,
        ));
        set = set.child(
            div()
                .flex()
                .gap_2()
                .child(button("Edit settings file", true, cx, |d, _| {
                    if let Err(e) = crate::win::open_in_notepad(&d.settings_path) {
                        d.message = Some(e);
                    }
                }))
                .child(button("Reload settings", true, cx, |d, cx| d.reload_settings(cx))),
        );
        set = set.child(note(self.settings_path.display().to_string(), MUTED));

        let gpu = card("GPU mining")
            .child(note("GPU mining: not supported yet.", MUTED))
            .child(button("Enable GPU mining", false, cx, |_, _| {}));

        let mut root = div()
            .id("root")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_xl().font_weight(FontWeight::BOLD).child("Lottery Miner"))
                    .child(
                        div()
                            .text_xl()
                            .text_color(rgb(ACCENT))
                            .child(fmt_hr(combined.filter(|c| *c > 0.0))),
                    ),
            );
        if let Some(m) = &self.message {
            root = root.child(
                div()
                    .flex()
                    .justify_between()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x3b2a20))
                    .child(div().text_sm().text_color(rgb(WARN)).child(m.clone()))
                    .child(button("Dismiss", true, cx, |d, _| d.message = None)),
            );
        }
        root.child(
            div()
                .flex()
                .gap_3()
                .w_full()
                .child(column().child(local).child(oxy).child(gpu))
                .child(column().child(odds_card).child(pool).child(set)),
        )
    }
}

/// Half-width column that may shrink below its content's natural width.
fn column() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .flex_1()
        .flex_basis(relative(0.))
        .min_w(px(0.))
}

fn short_addr(a: &str) -> String {
    if a.len() > 16 {
        format!("{}...{}", &a[..8], &a[a.len() - 6..])
    } else {
        a.to_string()
    }
}
