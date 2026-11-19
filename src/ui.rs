//! GPUI rendering: header, Overview / Settings / Logs tabs.
//!
//! Overview is status only (summary tiles and compact cards); every form
//! lives on the Settings tab.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, Context, Entity, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Window,
    div, prelude::*, px, relative, rgb, rgba,
};

use crate::app::{Dashboard, View};
use crate::input::TextInput;
use crate::logbook::{self, Filter, Source};
use crate::network::{self, Network};
use crate::odds;
use crate::remote::ServiceAction;
use crate::settings::{Coin, IDLE_STEPS, MAX_MHS_PER_INSTANCE};

// ---------------------------------------------------------------- palette

const BG: u32 = 0x0f1115;
const SURFACE: u32 = 0x171a21;
const SURFACE_2: u32 = 0x1f232c;
const SURFACE_3: u32 = 0x2a2f3a;
const BORDER: u32 = 0x262b36;
const TEXT: u32 = 0xe6e8ec;
const MUTED: u32 = 0x8b93a3;
const FAINT: u32 = 0x5c6373;
const BTC: u32 = 0xf7931a;
const XMR: u32 = 0xf26822;
const GOOD: u32 = 0x3fb950;
const WARN: u32 = 0xd29922;
const ERROR: u32 = 0xf85149;
const ON_ACCENT: u32 = 0x14110c;

/// From this window width (logical px) the Bitcoin and Monero cards sit side by side.
const WIDE_LAYOUT_MIN: f32 = 1500.0;

/// A color at about 15% opacity, for pill and banner backgrounds.
fn tint(color: u32) -> gpui::Rgba {
    rgba((color << 8) | 0x26)
}

// ------------------------------------------------------------- formatting

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
    format!("{} ago", fmt_duration(Duration::from_secs((now - unix).max(0) as u64)))
}

fn fmt_hr(hs: Option<f64>) -> String {
    hs.filter(|h| *h > 0.0).map(odds::format_hashrate).unwrap_or_else(|| "-".into())
}

/// Share difficulties and counts: "75,000", "1.73 billion".
fn fmt_count(v: f64) -> String {
    for (div, suffix) in [(1e15, "quadrillion"), (1e12, "trillion"), (1e9, "billion"), (1e6, "million")] {
        if v >= div {
            return format!("{:.2} {suffix}", v / div);
        }
    }
    let n = v.round() as u64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Thousands separators, no decimals: "EUR 181,923".
fn fmt_eur(v: f64) -> String {
    let s = (v.round() as i64).abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("EUR {out}")
}

fn fmt_minutes(m: u32) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn short_addr(a: &str) -> String {
    if a.len() > 16 {
        format!("{}...{}", &a[..8], &a[a.len() - 6..])
    } else {
        a.to_string()
    }
}

/// "1 in 19.3 million / year" for a Bitcoin hashrate, or None.
fn btc_odds_text(hs: f64, net: &Network) -> Option<String> {
    if hs <= 0.0 || net.difficulty.is_none() {
        return None;
    }
    network::odds_for(hs, net).one_in_per_year.map(odds::format_one_in)
}

// ------------------------------------------------------------- components

type Action = dyn Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Secondary,
    Danger,
}

fn btn(
    label: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    cx: &Context<Dashboard>,
    f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    let (bg, fg, hover) = match kind {
        Kind::Primary => (BTC, ON_ACCENT, 0xffa940),
        Kind::Secondary => (SURFACE_2, TEXT, SURFACE_3),
        Kind::Danger => (SURFACE_2, ERROR, SURFACE_3),
    };
    let el = div()
        .px_3()
        .py_1()
        .rounded_md()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .border_1()
        .border_color(rgb(if kind == Kind::Primary { bg } else { BORDER }))
        .child(label.into());
    if enabled {
        el.bg(rgb(bg))
            .text_color(rgb(fg))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    f(this, cx);
                    cx.notify();
                }),
            )
            .into_any_element()
    } else {
        el.bg(rgb(SURFACE)).text_color(rgb(FAINT)).into_any_element()
    }
}

/// Small rounded status label.
fn pill(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .bg(tint(color))
        .text_color(rgb(color))
        .child(text.into())
        .into_any_element()
}

/// On/off switch.
fn switch(on: bool, cx: &Context<Dashboard>, f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static) -> AnyElement {
    let knob = div().size(px(14.)).rounded_full().bg(rgb(if on { ON_ACCENT } else { MUTED }));
    let track = div()
        .flex_none()
        .w(px(34.))
        .h(px(20.))
        .p(px(3.))
        .rounded_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .bg(rgb(if on { BTC } else { SURFACE_3 }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                f(this, cx);
                cx.notify();
            }),
        );
    if on { track.justify_end() } else { track.justify_start() }
        .child(knob)
        .into_any_element()
}

/// Label (and optional hint) on the left, a control on the right.
fn setting_row(label: &str, hint: Option<&str>, control: AnyElement) -> AnyElement {
    let mut text = div().flex().flex_col().min_w(px(0.)).child(div().text_sm().child(label.to_string()));
    if let Some(h) = hint {
        text = text.child(div().text_xs().text_color(rgb(MUTED)).child(h.to_string()));
    }
    div()
        .flex()
        .justify_between()
        .items_center()
        .gap_4()
        .py_1()
        .child(text)
        .child(control)
        .into_any_element()
}

fn toggle_row(
    label: &str,
    hint: Option<&str>,
    on: bool,
    cx: &Context<Dashboard>,
    f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    setting_row(label, hint, switch(on, cx, f))
}

fn stepper(
    value: String,
    cx: &Context<Dashboard>,
    dec: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
    inc: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(btn("-", Kind::Secondary, true, cx, dec))
        .child(div().w(px(48.)).text_center().text_sm().font_weight(FontWeight::MEDIUM).child(value))
        .child(btn("+", Kind::Secondary, true, cx, inc))
        .into_any_element()
}

/// Label above a text input, with a short hint below.
fn field(label: &str, input: Entity<TextInput>, help: impl Into<SharedString>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .py_1()
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(label.to_uppercase()))
        .child(input)
        .child(div().text_xs().text_color(rgb(FAINT)).child(help.into()))
        .into_any_element()
}

/// Label / value line.
fn kv(label: impl Into<SharedString>, value: impl Into<SharedString>) -> AnyElement {
    kv_colored(label, value, TEXT)
}

fn kv_colored(label: impl Into<SharedString>, value: impl Into<SharedString>, color: u32) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .text_sm()
        .child(div().flex_none().text_color(rgb(MUTED)).child(label.into()))
        // The value takes the rest of the row, so it never collapses to a sliver.
        .child(div().flex_1().min_w(px(0.)).text_right().text_color(rgb(color)).child(value.into()))
        .into_any_element()
}

fn note(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().text_xs().text_color(rgb(color)).child(text.into()).into_any_element()
}

fn divider() -> AnyElement {
    div().h_px().my_1().bg(rgb(BORDER)).into_any_element()
}

/// A card with a title row; `right` goes on the title's right (pill, switch...).
fn card(title: &str, right: Option<AnyElement>) -> gpui::Div {
    // The title may wrap; whatever sits on the right keeps its size and stays inside.
    let mut head = div()
        .flex()
        .justify_between()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        );
    if let Some(r) = right {
        head = head.child(div().flex_none().child(r));
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded_lg()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .child(head)
}

/// Summary tile: small label, big value, one line of detail.
fn tile(label: &str, value: impl Into<SharedString>, sub: impl Into<SharedString>, accent: u32) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .flex_basis(relative(0.))
        .min_w(px(0.))
        .p_4()
        .rounded_lg()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(8.)).rounded_full().bg(rgb(accent)))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(label.to_uppercase())),
        )
        .child(div().text_2xl().font_weight(FontWeight::BOLD).child(value.into()))
        .child(div().text_xs().text_color(rgb(MUTED)).child(sub.into()))
        .into_any_element()
}

fn progress_bar(fraction: f64, color: u32) -> AnyElement {
    div()
        .w_full()
        .h(px(6.))
        .rounded_full()
        .bg(rgb(SURFACE_3))
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(rgb(color))
                .w(relative(fraction.clamp(0.0, 1.0) as f32)),
        )
        .into_any_element()
}

/// "● BTC 2.01 GH/s" in the header; fixed width so the layout stays put.
fn rate_readout(coin: &str, hs: f64, color: u32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .flex_none()
        .min_w(px(110.))
        .child(div().flex_none().size(px(8.)).rounded_full().bg(rgb(if hs > 0.0 { color } else { FAINT })))
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(coin.to_string()))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if hs > 0.0 { TEXT } else { FAINT }))
                .child(fmt_hr(Some(hs))),
        )
        .into_any_element()
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

fn save_row(label: &str, cx: &Context<Dashboard>, f: impl Fn(&mut Dashboard, &mut Context<Dashboard>) + 'static) -> AnyElement {
    div()
        .flex()
        .justify_end()
        .pt_1()
        .child(btn(label.to_string(), Kind::Primary, true, cx, f))
        .into_any_element()
}

/// Logical CPUs on this PC (upper limit for XMRig's threads).
fn max_cpu_threads() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(16)
}

// ------------------------------------------------------------------ render

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window width in logical pixels, for the few layout choices that depend on it.
        let wide = window.viewport_size().width >= px(WIDE_LAYOUT_MIN);
        let mut root = div()
            .id("root")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.header(cx));
        if let Some(m) = &self.message {
            root = root.child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(tint(WARN))
                    .border_1()
                    .border_color(rgba((WARN << 8) | 0x55))
                    .child(div().text_sm().text_color(rgb(TEXT)).child(m.clone()))
                    .child(btn("Dismiss", Kind::Secondary, true, cx, |d, _| d.message = None)),
            );
        }
        root = root.child(tab_bar(self.view, cx));
        match self.view {
            View::Dashboard => root.child(self.overview(wide, cx)),
            View::Settings => root.child(self.settings_view(cx)),
            View::Logs => root.child(self.logs_view(cx)),
        }
    }
}

impl Dashboard {
    fn header(&self, cx: &Context<Self>) -> AnyElement {
        let cpu_on = self.miner.is_running();
        let gpu_on = self.gpu_miner.is_running();
        let quick_hint = self.settings.quick_start_hint();
        let quick_ok = !(cpu_on && gpu_on) && quick_hint.is_none();
        let mining = cpu_on || gpu_on;
        let mut title = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(28.))
                    .rounded_full()
                    .bg(rgb(BTC))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(ON_ACCENT))
                    .font_weight(FontWeight::BOLD)
                    .child("B"),
            )
            .child(div().text_lg().font_weight(FontWeight::BOLD).child("Lottery Miner"));
        title = title.child(if mining { pill("Mining", GOOD) } else { pill("Not mining", FAINT) });
        // Per-coin totals (this PC and the home server together).
        let btc_total = self.total_hashrate() + self.oxyhome_hashrate().unwrap_or(0.0);
        let xmr_total = self.xmr_local_hashrate() + self.oxyhome_xmr_hashrate().unwrap_or(0.0);
        let rates = div()
            .flex()
            .items_center()
            .gap_4()
            .pl_2()
            .child(rate_readout("BTC", btc_total, BTC))
            .child(rate_readout("XMR", xmr_total, XMR));
        let mut controls = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .items_center()
            .gap_2()
            .child(coin_switch(self.settings.coin, cx))
            .child(btn("Quick start", Kind::Primary, quick_ok, cx, |d, _| d.quick_start()))
            .child(btn("Stop all (this PC)", Kind::Secondary, mining, cx, |d, _| d.stop_local_miners()));
        if let Some(h) = quick_hint.filter(|_| !(cpu_on && gpu_on)) {
            controls = controls.child(div().max_w(px(220.)).child(note(h, WARN)));
        }
        controls = controls.child(rates);
        div()
            .flex()
            .flex_wrap()
            .justify_between()
            .items_center()
            .gap_4()
            .child(title)
            .child(controls)
            .into_any_element()
    }

    // -------------------------------------------------------------- overview

    fn overview(&self, wide: bool, cx: &Context<Self>) -> AnyElement {
        let net = self.network_numbers();
        let btc_local = self.total_hashrate();
        let btc_oxy = self.oxyhome_hashrate();
        let btc_total = btc_local + btc_oxy.unwrap_or(0.0);
        let xmr_local = self.xmr_local_hashrate();
        let xmr_oxy = self.oxyhome_xmr_hashrate();
        let xmr_total = xmr_local + xmr_oxy.unwrap_or(0.0);

        // Tiles
        let cpu_btc = if self.cpu_coin == Coin::Bitcoin { self.cpu_hashrate() } else { 0.0 };
        let t_btc = tile(
            "Bitcoin, this PC",
            fmt_hr(Some(btc_local)),
            format!("GPU {} / CPU {}", fmt_hr(Some(self.gpu_hashrate())), fmt_hr(Some(cpu_btc))),
            BTC,
        );
        let xmr_sub = match &self.xmr_net {
            Some(Ok(n)) => match network::xmr_per_day(xmr_total, n) {
                Some(d) => format!("about EUR {:.2}/day, EUR {:.0}/year", d * n.eur_per_xmr.unwrap_or(0.0), d * 365.0 * n.eur_per_xmr.unwrap_or(0.0)),
                None => "not mining Monero".into(),
            },
            _ => "loading network numbers".into(),
        };
        let t_xmr = tile("Monero, total", fmt_hr(Some(xmr_total)), xmr_sub, XMR);
        let server = self.server_name();
        let (oxy_value, oxy_sub, oxy_color) = match &self.oxyhome {
            None => ("-".to_string(), "connecting...".to_string(), FAINT),
            Some(Err(_)) => ("offline".to_string(), "cannot connect over SSH".to_string(), ERROR),
            Some(Ok(s)) => {
                let coin = if s.unit == "xmrig" { "Monero" } else { "Bitcoin" };
                let mut sub = if s.service_active { coin.to_string() } else { format!("{coin} miner stopped") };
                if let Some(t) = s.temp_c {
                    sub.push_str(&format!(", {t:.0} C"));
                }
                if let Some(w) = s.watts {
                    sub.push_str(&format!(", {w:.0} W"));
                }
                (fmt_hr(s.log.hashrate.filter(|_| s.service_active)), sub, if s.unit == "xmrig" { XMR } else { BTC })
            }
        };
        let t_oxy = tile(&server, oxy_value, oxy_sub, oxy_color);
        let t_odds = match btc_odds_text(btc_total, &net) {
            Some(o) => tile(
                "Bitcoin block odds",
                o,
                format!(
                    "per year, combined{}",
                    match (net.avg_fees_sats, net.eur_per_btc) {
                        (Some(f), Some(e)) => format!(" / prize {}", fmt_eur(odds::prize_eur(f, e))),
                        _ => String::new(),
                    }
                ),
                GOOD,
            ),
            None => tile("Bitcoin block odds", "-", "not mining Bitcoin", GOOD),
        };
        let tiles = div().flex().gap_3().child(t_btc).child(t_xmr).child(t_oxy).child(t_odds);

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(tiles)
            .child(
                div()
                    .flex()
                    .gap_3()
                    .w_full()
                    .child(column().child(self.this_pc_card(cx)).child(self.idle_card(cx)))
                    .child({
                        let btc = self.bitcoin_card(&net, btc_local, btc_oxy, cx);
                        let xmr = self.monero_card(xmr_local, xmr_oxy, xmr_total);
                        let right = column().child(self.server_card(cx));
                        if wide {
                            // The two coins side by side under the server.
                            right.child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .w_full()
                                    .child(column().child(btc))
                                    .child(column().child(xmr)),
                            )
                        } else {
                            // Not enough room: stack them.
                            right.child(btc).child(xmr)
                        }
                    }),
            )
            .into_any_element()
    }

    fn server_name(&self) -> String {
        let t = &self.settings.oxyhome;
        if t.name.is_empty() { t.host.clone() } else { t.name.clone() }
    }

    /// One miner line: name, pills, rate, buttons.
    fn miner_line(
        &self,
        name: &str,
        coin: Coin,
        status: (String, u32),
        rate: Option<f64>,
        detail: String,
        running: bool,
        cx: &Context<Self>,
        start: Box<Action>,
        stop: Box<Action>,
    ) -> AnyElement {
        let coin_pill = match coin {
            Coin::Bitcoin => pill("Bitcoin", BTC),
            Coin::Monero => pill("Monero", XMR),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .py_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(name.to_string()))
                            .child(coin_pill)
                            .child(pill(status.0, status.1)),
                    )
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child(fmt_hr(rate))),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(div().min_w(px(0.)).child(note(detail, MUTED)))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(btn("Start", Kind::Secondary, !running, cx, move |d, cx| start(d, cx)))
                            .child(btn("Stop", Kind::Secondary, running, cx, move |d, cx| stop(d, cx))),
                    ),
            )
            .into_any_element()
    }

    fn this_pc_card(&self, cx: &Context<Self>) -> AnyElement {
        let cpu_running = self.miner.is_running();
        let cpu_coin = if cpu_running { self.cpu_coin } else { self.settings.coin };
        let cpu_status = if cpu_running {
            if self.local.iter().any(|v| v.stalled_now) {
                ("Waiting for work".to_string(), WARN)
            } else {
                ("Running".to_string(), GOOD)
            }
        } else {
            ("Stopped".to_string(), FAINT)
        };
        let shares: (u64, u64) = self.local.iter().fold((0, 0), |a, v| (a.0 + v.accepted, a.1 + v.rejected));
        let cpu_detail = if cpu_running {
            let what = match cpu_coin {
                Coin::Bitcoin => format!("{} cpuminer instances", self.local.len().max(self.miner.handles().len())),
                Coin::Monero => match self.settings.monero.threads {
                    0 => "XMRig, all threads".to_string(),
                    n => format!("XMRig, {n} threads"),
                },
            };
            let up = self.local.iter().map(|v| v.uptime).max().unwrap_or_default();
            format!("{what}, {} accepted / {} rejected, up {}", shares.0, shares.1, fmt_duration(up))
        } else {
            match cpu_coin {
                Coin::Bitcoin => "cpuminer-opt, ckpool solo".into(),
                Coin::Monero => "XMRig, Monero pool".into(),
            }
        };
        let cpu = self.miner_line(
            "CPU",
            cpu_coin,
            cpu_status,
            Some(self.cpu_hashrate()),
            cpu_detail,
            cpu_running,
            cx,
            Box::new(|d, _| d.start_mining()),
            Box::new(|d, _| d.stop_mining()),
        );

        let gpu_running = self.gpu_miner.is_running();
        let (gpu_status, gpu_detail) = match (&self.gpu, gpu_running) {
            (_, false) => (("Stopped".to_string(), FAINT), "gpu-miner, always Bitcoin".to_string()),
            (None, true) => (("Starting".to_string(), WARN), "running the GPU self-test".to_string()),
            (Some(v), true) => {
                let status = if v.connected {
                    ("Running".to_string(), GOOD)
                } else if v.last_error.is_some() {
                    ("Problem".to_string(), WARN)
                } else {
                    ("Connecting".to_string(), WARN)
                };
                let detail = match &v.last_error {
                    Some(e) if !v.connected => e.clone(),
                    _ => format!(
                        "{}, {} accepted / {} rejected, up {}",
                        v.device.clone().unwrap_or_else(|| "GPU".into()),
                        v.accepted,
                        v.rejected,
                        fmt_duration(v.uptime)
                    ),
                };
                (status, detail)
            }
        };
        let gpu = self.miner_line(
            "GPU",
            Coin::Bitcoin,
            gpu_status,
            Some(self.gpu_hashrate()),
            gpu_detail,
            gpu_running,
            cx,
            Box::new(|d, _| d.start_gpu()),
            Box::new(|d, _| d.stop_gpu()),
        );

        let mut c = card("This PC", None).child(cpu).child(divider()).child(gpu);
        // Warnings, only when something is wrong.
        let stalls: u32 = self.local.iter().map(|v| v.stall_count).sum();
        if cpu_coin == Coin::Bitcoin && stalls > 0 {
            c = c.child(note(format!("cpuminer ran out of nonces {stalls} time(s) and waited for a new job."), WARN));
        }
        if let Some(v) = self.local.iter().find(|v| v.hashrate.unwrap_or(0.0) > MAX_MHS_PER_INSTANCE * 1e6) {
            c = c.child(note(
                format!("Instance #{} is above {MAX_MHS_PER_INSTANCE:.0} MH/s and will idle between jobs; use more instances with fewer threads.", v.index + 1),
                WARN,
            ));
        }
        if cpu_running && !self.local.is_empty() && self.local.iter().all(|v| v.api_error.is_some()) {
            let ports: Vec<String> = self.local.iter().map(|v| v.port.to_string()).collect();
            c = c.child(note(format!("Miner stats not reachable yet (port {}); showing log values.", ports.join(", ")), MUTED));
        }
        c.into_any_element()
    }

    fn idle_card(&self, cx: &Context<Self>) -> AnyElement {
        let is = &self.settings.idle;
        let toggle_enabled = switch(is.enabled, cx, |d, _| {
            d.settings.idle.enabled = !d.settings.idle.enabled;
            d.save_settings();
        });
        let idle_for = fmt_duration(Duration::from_secs(self.idle_secs));
        let (status, color) = if !is.enabled {
            ("Off".to_string(), FAINT)
        } else if self.idle_mining() {
            (format!("Mining while away ({idle_for} without input)"), GOOD)
        } else if !is.cpu && !is.gpu {
            ("Pick the CPU, the GPU or both".to_string(), WARN)
        } else {
            (format!("Starts after {} without input (now {idle_for})", fmt_minutes(is.minutes)), MUTED)
        };
        card("Idle mining", Some(toggle_enabled))
            .child(note(status, color))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().text_color(rgb(MUTED)).child("Start after"))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if is.enabled { BTC } else { MUTED }))
                            .child(fmt_minutes(is.minutes)),
                    ),
            )
            .child(idle_slider(is.minutes, cx))
            .child(toggle_row("Use the CPU", None, is.cpu, cx, |d, _| {
                d.settings.idle.cpu = !d.settings.idle.cpu;
                d.save_settings();
            }))
            .child(toggle_row("Use the GPU", None, is.gpu, cx, |d, _| {
                d.settings.idle.gpu = !d.settings.idle.gpu;
                d.save_settings();
            }))
            .child(toggle_row("Keep the PC awake", Some("Only while idle mining runs"), is.keep_awake, cx, |d, _| {
                d.settings.idle.keep_awake = !d.settings.idle.keep_awake;
                d.save_settings();
            }))
            .child(note("Stops on the next mouse or keyboard input. Miners you started yourself keep running.", FAINT))
            .into_any_element()
    }

    fn server_card(&self, cx: &Context<Self>) -> AnyElement {
        let busy = self.oxyhome_busy;
        let can_control = !busy && self.oxyhome_unit_present();
        let status_pill = match &self.oxyhome {
            None => pill("Connecting", FAINT),
            Some(Err(_)) => pill("Offline", ERROR),
            Some(Ok(s)) if !s.unit_found => pill("No miner", WARN),
            Some(Ok(s)) if s.service_active => pill("Running", GOOD),
            Some(Ok(_)) => pill("Stopped", WARN),
        };
        let mut c = card(&format!("{} (home server)", self.server_name()), Some(status_pill));
        match &self.oxyhome {
            None => c = c.child(note("Connecting over SSH...", MUTED)),
            Some(Err(e)) => c = c.child(note(e.clone(), ERROR)),
            Some(Ok(s)) => {
                let coin = if s.unit == "xmrig" { "Monero (xmrig)" } else { "Bitcoin (cpuminer)" };
                c = c
                    .child(kv("Mining", coin))
                    .child(kv("Hashrate", fmt_hr(s.log.hashrate.filter(|_| s.service_active))))
                    .child(kv("Shares", format!("{} accepted / {} rejected", s.log.accepted, s.log.rejected())))
                    .child(kv(
                        "CPU / power",
                        format!(
                            "{} / {}",
                            s.temp_c.map(|t| format!("{t:.0} C")).unwrap_or_else(|| "-".into()),
                            s.watts.map(|w| format!("{w:.0} W")).unwrap_or_else(|| "-".into())
                        ),
                    ));
                if let Some(since) = s.active_since.as_ref().filter(|v| !v.is_empty()) {
                    c = c.child(kv("Running since", since.clone()));
                }
                if let Some(n) = &s.node {
                    c = c.child(divider());
                    if n.synchronized {
                        c = c
                            .child(kv_colored("Monero node", format!("synced, {} peers", n.out_peers + n.in_peers), GOOD))
                            .child(note(format!("Wallets on your network: {}:18089", self.settings.oxyhome.host.split('@').last().unwrap_or("oxyhome")), FAINT));
                    } else {
                        let left = match self.node_rate.filter(|r| *r > 0.0) {
                            Some(rate) => format!(
                                ", about {} left",
                                fmt_duration(Duration::from_secs_f64(n.remaining_blocks() as f64 / rate))
                            ),
                            None => String::new(),
                        };
                        c = c
                            .child(kv_colored(
                                "Monero node",
                                format!("syncing {:.1}%{left}", n.progress() * 100.0),
                                WARN,
                            ))
                            .child(progress_bar(n.progress(), XMR))
                            .child(note(
                                format!(
                                    "Block {} of {}, {} peers, {:.1} GB",
                                    n.height,
                                    n.target_height,
                                    n.out_peers + n.in_peers,
                                    n.database_bytes as f64 / 1e9
                                ),
                                FAINT,
                            ));
                    }
                }
            }
        }
        c.child(
            div()
                .flex()
                .gap_2()
                .pt_1()
                .child(btn("Start", Kind::Secondary, can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Start, cx)))
                .child(btn("Stop", Kind::Danger, can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Stop, cx)))
                .child(btn("Restart", Kind::Secondary, can_control, cx, |d, cx| d.oxyhome_control(ServiceAction::Restart, cx)))
                .child(if busy { note("Working...", MUTED) } else { div().into_any_element() }),
        )
        .into_any_element()
    }

    fn bitcoin_card(&self, net: &Network, local: f64, oxy: Option<f64>, cx: &Context<Self>) -> AnyElement {
        let pool_url = network::pool_user_url(&self.settings.address);
        let mut c = card(
            "Bitcoin (ckpool solo)",
            Some(btn("Open on ckpool", Kind::Secondary, true, cx, move |_, cx| cx.open_url(&pool_url))),
        );
        let odds_line = |hs: f64| btc_odds_text(hs, net).map(|o| format!("{o} / year")).unwrap_or_else(|| "not mining".into());
        c = c
            .child(kv("This PC", odds_line(local)))
            .child(kv(self.server_name(), odds_line(oxy.unwrap_or(0.0))))
            .child(kv("Combined", odds_line(local + oxy.unwrap_or(0.0))));
        if let (Some(f), Some(e)) = (net.avg_fees_sats, net.eur_per_btc) {
            let btc = (odds::SUBSIDY_BTC + f / 1e8) * (1.0 - odds::POOL_FEE);
            c = c.child(kv("Prize (after 2% fee)", format!("{btc:.3} BTC = {}", fmt_eur(odds::prize_eur(f, e)))));
        }
        if let Some(t) = odds::seconds_per_pool_share(local) {
            c = c.child(kv("Pool share (this PC)", format!("about every {}", fmt_duration(Duration::from_secs_f64(t)))));
        }
        c = c.child(divider());
        match &self.pool {
            None => c = c.child(note("Loading pool stats...", MUTED)),
            Some(Err(e)) => c = c.child(note(e.clone(), ERROR)),
            Some(Ok(None)) => c = c.child(note("No accepted share yet (normal until the first one).", MUTED)),
            Some(Ok(Some(u))) => {
                if let Some(b) = u.best_share {
                    let short = net
                        .difficulty
                        .and_then(|d| odds::times_short(d, b))
                        .map(|n| format!(", {n:.1e} times short"))
                        .unwrap_or_default();
                    c = c.child(kv("Best share", format!("{b:.0}{short}")));
                }
                if let Some(t) = u.last_share_unix {
                    c = c.child(kv("Last share", fmt_ago(t)));
                }
                for w in &u.workers {
                    let name = w.name.rsplit('.').next().unwrap_or(&w.name).to_string();
                    c = c.child(kv(
                        format!("  {name}"),
                        format!("last share {}", w.last_share_unix.map(fmt_ago).unwrap_or_else(|| "-".into())),
                    ));
                }
            }
        }
        if let Some(Err(e)) = &self.network {
            c = c.child(note(format!("mempool.space: {e}"), ERROR));
        }
        c.into_any_element()
    }

    fn monero_card(&self, local: f64, oxy: Option<f64>, total: f64) -> AnyElement {
        let mut c = card(
            "Monero (pool)",
            Some(if self.settings.coin == Coin::Monero { pill("CPUs on Monero", XMR) } else { pill("CPUs on Bitcoin", FAINT) }),
        )
        .child(kv("This PC", fmt_hr(Some(local))))
        .child(kv(self.server_name(), fmt_hr(oxy)))
        .child(kv("Total", fmt_hr(Some(total))));
        c = c.child(divider());
        c = self.monero_shares(c);
        c = c.child(divider());
        match &self.xmr_net {
            None => c = c.child(note("Loading Monero network numbers...", MUTED)),
            Some(Err(e)) => c = c.child(note(format!("Monero stats: {e}"), ERROR)),
            Some(Ok(n)) => {
                let eur = n.eur_per_xmr.unwrap_or(0.0);
                if let Some(d) = network::xmr_per_day(total, n) {
                    c = c
                        .child(kv("Expected per day", format!("{d:.5} XMR = EUR {:.2}", d * eur)))
                        .child(kv("Expected per year", format!("{:.3} XMR = EUR {:.0}", d * 365.0, d * 365.0 * eur)));
                }
                if let Some(h) = n.hashrate {
                    c = c.child(kv("Network", odds::format_hashrate(h)));
                }
                if let Some(r) = n.reward_xmr {
                    c = c.child(kv("Block reward", format!("{r:.4} XMR = EUR {:.0}", r * eur)));
                }
                if eur > 0.0 {
                    c = c.child(kv("Price", format!("1 XMR = EUR {eur:.2}")));
                }
            }
        }
        c.into_any_element()
    }

    /// Share stats for the Monero card, like the Bitcoin card's: this PC's XMRig
    /// (when the CPUs are on Monero), the home server's XMRig, then SupportXMR.
    fn monero_shares(&self, mut c: gpui::Div) -> gpui::Div {
        let net_diff = self.xmr_net.as_ref().and_then(|r| r.as_ref().ok()).and_then(|n| n.difficulty);
        let best_text = |b: f64| {
            let short = net_diff
                .and_then(|d| odds::times_short(d, b))
                .map(|n| format!(", {n:.1e} times short"))
                .unwrap_or_default();
            format!("{}{short}", fmt_count(b))
        };
        let every = |target: Option<f64>, hs: f64| {
            target
                .filter(|t| *t > 0.0 && hs > 0.0)
                .map(|t| format!("about every {}", fmt_duration(Duration::from_secs_f64(t / hs))))
        };
        let mut any = false;

        if self.miner.is_running() && self.cpu_coin == Coin::Monero {
            let best = self.local.iter().filter_map(|v| v.best_share_diff).reduce(f64::max);
            let last = self.local.iter().filter_map(|v| v.last_share_unix).reduce(f64::max);
            let target = self.local.iter().find_map(|v| v.pool_diff);
            if let Some(e) = every(target, self.cpu_hashrate()) {
                c = c.child(kv("Pool share (this PC)", e));
            }
            if let Some(b) = best {
                c = c.child(kv("Best share (this PC)", best_text(b)));
            }
            c = c.child(kv("Last share (this PC)", last.map(|t| fmt_ago(t as i64)).unwrap_or_else(|| "none yet".into())));
            any = true;
        }

        if let Some(Ok(s)) = &self.oxyhome {
            if s.unit == "xmrig" && s.service_active {
                let name = self.server_name();
                let target = s.xmrig.as_ref().and_then(|x| x.difficulty).or(s.log.difficulty);
                let hs = s.xmrig.as_ref().map(|x| x.hashrate).or(s.log.hashrate).unwrap_or(0.0);
                if let Some(e) = every(target, hs) {
                    c = c.child(kv(format!("Pool share ({name})"), e));
                }
                if let Some(b) = s.xmrig.as_ref().and_then(|x| x.best_share) {
                    c = c.child(kv(format!("Best share ({name})"), best_text(b)));
                }
                if let Some(t) = s.last_share_unix {
                    c = c.child(kv(format!("Last share ({name})"), fmt_ago(t as i64)));
                }
                any = true;
            }
        }

        match &self.xmr_pool {
            Some(Ok(Some(p))) => {
                c = c.child(kv(
                    "SupportXMR shares",
                    format!("{} valid / {} invalid", fmt_count(p.valid_shares as f64), p.invalid_shares),
                ));
                if let Some(t) = p.last_share_unix {
                    c = c.child(kv("SupportXMR last share", fmt_ago(t)));
                }
                c = c.child(kv("Pending payout", format!("{:.6} XMR", p.due_xmr)));
            }
            Some(Err(e)) => c = c.child(note(format!("SupportXMR: {e}"), ERROR)),
            _ if !any => c = c.child(note("No Monero miner running.", MUTED)),
            _ => {}
        }
        c
    }

    // -------------------------------------------------------------- settings

    fn settings_view(&self, cx: &Context<Self>) -> AnyElement {
        let s = &self.settings;
        let f = &self.form;

        // Bitcoin CPU miner
        let per_instance = 290.0 / 16.0 * s.threads_per_instance as f64;
        let btc = card("Bitcoin CPU miner (cpuminer-opt)", None)
            .child(field("Miner executable", f.miner_exe.clone(), "cpuminer-opt build for this CPU"))
            .child(field("Bitcoin address", f.address.clone(), "Payouts and pool user name"))
            .child(field("Pool", f.pool_url.clone(), "ckpool solo, EU"))
            .child(field("Worker name", f.worker.clone(), format!("Pool user: {}.<worker>", short_addr(&s.address))))
            .child(save_row("Save", cx, |d, cx| d.apply_miner_form(cx)))
            .child(divider())
            .child(setting_row(
                "Instances",
                None,
                stepper(s.instances.to_string(), cx, |d, _| {
                    d.settings.instances = d.settings.instances.saturating_sub(1).max(1);
                    d.save_settings();
                }, |d, _| {
                    if (d.settings.instances + 1) * d.settings.threads_per_instance <= 16 {
                        d.settings.instances += 1;
                        d.save_settings();
                    }
                }),
            ))
            .child(setting_row(
                "Threads per instance",
                None,
                stepper(s.threads_per_instance.to_string(), cx, |d, _| {
                    d.settings.threads_per_instance = d.settings.threads_per_instance.saturating_sub(1).max(1);
                    d.save_settings();
                }, |d, _| {
                    if d.settings.instances * (d.settings.threads_per_instance + 1) <= 16 {
                        d.settings.threads_per_instance += 1;
                        d.save_settings();
                    }
                }),
            ))
            .child(note(
                format!("About {per_instance:.0} MH/s per instance; keep it under {MAX_MHS_PER_INSTANCE:.0}. API ports {}-{}.", s.api_base_port, s.api_base_port as u32 + s.instances.saturating_sub(1)),
                if per_instance > MAX_MHS_PER_INSTANCE { WARN } else { FAINT },
            ));

        // GPU
        let gs = &s.gpu;
        let gpu = card("GPU miner (always Bitcoin)", None)
            .child(setting_row(
                "Intensity",
                Some("Lower it if games or video stutter"),
                stepper(gs.intensity.to_string(), cx, |d, _| {
                    d.settings.gpu.intensity = d.settings.gpu.intensity.saturating_sub(1).max(16);
                    d.save_settings();
                }, |d, _| {
                    d.settings.gpu.intensity = (d.settings.gpu.intensity + 1).min(30);
                    d.save_settings();
                }),
            ))
            .child(field("Worker name", f.gpu_worker.clone(), "Different from the CPU worker"))
            .child(field("Device", f.gpu_device.clone(), "Part of the GPU name; empty picks the first discrete GPU"))
            .child(save_row("Save", cx, |d, cx| d.apply_gpu_form(cx)))
            .child(toggle_row("Start GPU mining when the app starts", None, gs.start_on_launch, cx, |d, _| {
                d.settings.gpu.start_on_launch = !d.settings.gpu.start_on_launch;
                d.save_settings();
            }));

        // Monero
        let xmr = card("Monero (XMRig)", None)
            .child(setting_row(
                "Threads on this PC",
                Some("Fewer leaves room for your apps; applies on the next start"),
                stepper(
                    match s.monero.threads {
                        0 => "Auto".to_string(),
                        n => n.to_string(),
                    },
                    cx,
                    |d, _| {
                        // Auto (0) sits below 1.
                        d.settings.monero.threads = d.settings.monero.threads.saturating_sub(1);
                        d.save_settings();
                    },
                    |d, _| {
                        d.settings.monero.threads = (d.settings.monero.threads + 1).min(max_cpu_threads());
                        d.save_settings();
                    },
                ),
            ))
            .child(field("Monero address", f.xmr_address.clone(), "Payouts go here; needed before switching to Monero"))
            .child(field("Pool (TLS)", f.xmr_pool.clone(), "host:port, also used by the home server"))
            .child(field("XMRig executable", f.xmrig_exe.clone(), "Empty: tools\\xmrig\\xmrig.exe or next to this app"))
            .child(save_row("Save", cx, |d, cx| d.apply_monero_form(cx)));

        // Home server
        let ssh = card("Home server (SSH)", None)
            .child(field("Name", f.ssh_name.clone(), "Shown in the app"))
            .child(field("SSH host", f.ssh_host.clone(), "user@host, or a Host from ~/.ssh/config"))
            .child(field("SSH port", f.ssh_port.clone(), "Empty: 22 or your SSH config"))
            .child(field("SSH key (optional)", f.ssh_key.clone(), "Empty: your SSH config or agent"))
            .child(save_row("Save and connect", cx, |d, cx| d.apply_ssh_form(cx)))
            .child(note("The app only starts, stops or switches the miner services (cpuminer, xmrig) there.", FAINT));

        // App
        let app = card("App", None)
            .child(setting_row(
                "Refresh every",
                None,
                stepper(format!("{} s", s.refresh_secs), cx, |d, _| {
                    d.settings.refresh_secs = d.settings.refresh_secs.saturating_sub(1).max(2);
                    d.save_settings();
                }, |d, _| {
                    d.settings.refresh_secs = (d.settings.refresh_secs + 1).min(300);
                    d.save_settings();
                }),
            ))
            .child(toggle_row("Start CPU mining when the app starts", None, s.start_mining_on_launch, cx, |d, _| {
                d.settings.start_mining_on_launch = !d.settings.start_mining_on_launch;
                d.save_settings();
            }))
            .child(toggle_row("Start the app at login", None, s.start_at_login, cx, |d, _| {
                let v = !d.settings.start_at_login;
                d.set_start_at_login(v);
            }))
            .child(toggle_row(
                "Stop this PC's miners on Quit",
                Some("The home server is never stopped by quitting"),
                s.stop_miners_on_quit,
                cx,
                |d, _| {
                    d.settings.stop_miners_on_quit = !d.settings.stop_miners_on_quit;
                    d.save_settings();
                },
            ))
            .child(divider())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(btn("Edit settings file", Kind::Secondary, true, cx, |d, _| {
                        if let Err(e) = crate::win::open_in_notepad(&d.settings_path) {
                            d.message = Some(e);
                        }
                    }))
                    .child(btn("Reload settings", Kind::Secondary, true, cx, |d, cx| d.reload_settings(cx))),
            )
            .child(note(self.settings_path.display().to_string(), FAINT));

        let m = &s.metrics;
        let (status, color) = match (&self.metrics_status, m.enabled) {
            (_, false) => ("Off".to_string(), MUTED),
            (_, true) if m.password.is_empty() => ("No password in settings.json".to_string(), WARN),
            (_, true) if !m.ready() => ("URL must be https:// and instance one word".to_string(), WARN),
            (None, true) => ("Starting".to_string(), MUTED),
            (Some(Ok(t)), true) => {
                let ago = (crate::parse::now_unix() - t).max(0.0);
                (format!("Pushed {ago:.0} s ago"), GOOD)
            }
            (Some(Err(e)), true) => (e.clone(), ERROR),
        };
        let metrics = card("Grafana metrics", None)
            .child(toggle_row(
                "Push stats to Prometheus",
                Some("Every 15 s to the Pushgateway; deleted on Quit"),
                m.enabled,
                cx,
                |d, _| {
                    let v = !d.settings.metrics.enabled;
                    d.set_metrics_enabled(v);
                },
            ))
            .child(kv("Endpoint", m.group_url()))
            .child(kv_colored("Status", status, color))
            .child(note("URL, user and password: metrics section of settings.json", FAINT));

        div()
            .flex()
            .gap_3()
            .w_full()
            .child(column().child(btc).child(gpu).child(metrics))
            .child(column().child(xmr).child(ssh).child(app))
            .into_any_element()
    }

    // ------------------------------------------------------------------ logs

    fn logs_view(&self, cx: &Context<Self>) -> AnyElement {
        let filter = self.log_filter;
        let (lines, total) = match self.logbook.lock() {
            Ok(b) => (b.recent(filter, LOG_VIEW_LINES), b.len()),
            Err(_) => (Vec::new(), 0),
        };
        let chip = |label: &'static str, f: Option<Filter>| {
            let active = filter == f;
            div()
                .px_3()
                .py_0p5()
                .rounded_full()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .border_1()
                .border_color(rgb(if active { BTC } else { BORDER }))
                .text_color(rgb(if active { BTC } else { MUTED }))
                .bg(if active { tint(BTC) } else { rgb(SURFACE).into() })
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |d, _: &MouseDownEvent, _, cx| {
                        d.log_filter = f;
                        cx.notify();
                    }),
                )
        };
        let export = logbook::to_text(&lines);
        let controls = div()
            .flex()
            .justify_between()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(chip("All", None))
                    .child(chip("CPU", Some(Filter::Cpu)))
                    .child(chip("GPU", Some(Filter::Gpu)))
                    .child(chip("Server", Some(Filter::Server)))
                    .child(chip("App", Some(Filter::App))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(note(format!("UTC, {} of {} lines, newest first", lines.len(), total), FAINT))
                    .child(btn("Copy", Kind::Secondary, !lines.is_empty(), cx, move |_, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(export.clone()));
                    }))
                    .child(btn("Clear", Kind::Secondary, total > 0, cx, |d, _| {
                        if let Ok(mut b) = d.logbook.lock() {
                            b.clear();
                        }
                    })),
            );
        let mut list = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_3()
            .rounded_lg()
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .font_family("Consolas")
            .text_xs();
        if lines.is_empty() {
            list = list.child(div().text_color(rgb(MUTED)).child(
                "Nothing yet. Miner output, idle mode, server commands and the server's journal appear here.",
            ));
        }
        for l in &lines {
            list = list.child(
                div()
                    .flex()
                    .gap_3()
                    .child(div().flex_none().w(px(60.)).text_color(rgb(FAINT)).child(logbook::utc_time(l.at)))
                    .child(div().flex_none().w(px(64.)).text_color(rgb(source_color(l.source))).child(l.source.label()))
                    .child(div().min_w(px(0.)).text_color(rgb(TEXT)).child(l.text.clone())),
            );
        }
        div().flex().flex_col().gap_3().child(controls).child(list).into_any_element()
    }
}

/// Lines shown in the Logs tab (newest first).
const LOG_VIEW_LINES: usize = 500;

fn source_color(s: Source) -> u32 {
    match s {
        Source::Cpu(_) => 0x79a8ff,
        Source::Gpu => 0xc49bff,
        Source::App => BTC,
        Source::Server => GOOD,
    }
}

/// Segmented "Bitcoin | Monero" for the CPU miners here and on the home server.
fn coin_switch(current: Coin, cx: &Context<Dashboard>) -> AnyElement {
    let seg = |label: &'static str, coin: Coin, color: u32| {
        let active = current == coin;
        let el = div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |d, _: &MouseDownEvent, _, cx| {
                    d.set_coin(coin, cx);
                    cx.notify();
                }),
            );
        if active {
            el.bg(rgb(color)).text_color(rgb(ON_ACCENT))
        } else {
            el.text_color(rgb(MUTED)).hover(|s| s.text_color(rgb(TEXT)))
        }
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(note("CPUs", FAINT))
        .child(
            div()
                .flex()
                .p(px(3.))
                .gap_1()
                .rounded_lg()
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(BORDER))
                .child(seg("Bitcoin", Coin::Bitcoin, BTC))
                .child(seg("Monero", Coin::Monero, XMR)),
        )
        .into_any_element()
}

/// Underlined tabs.
fn tab_bar(view: View, cx: &Context<Dashboard>) -> AnyElement {
    let tab = |label: &'static str, this: View| {
        let active = view == this;
        div()
            .px_1()
            .pb_2()
            .cursor_pointer()
            .text_sm()
            .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
            .text_color(rgb(if active { TEXT } else { MUTED }))
            .border_b_2()
            .border_color(rgb(if active { BTC } else { BG }))
            .hover(|s| s.text_color(rgb(TEXT)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |d, _: &MouseDownEvent, _, cx| {
                    d.view = this;
                    cx.notify();
                }),
            )
    };
    div()
        .flex()
        .gap_5()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(tab("Overview", View::Dashboard))
        .child(tab("Settings", View::Settings))
        .child(tab("Logs", View::Logs))
        .into_any_element()
}

/// Stepped slider over IDLE_STEPS: click or drag along the track.
fn idle_slider(selected: u32, cx: &Context<Dashboard>) -> AnyElement {
    let sel = IDLE_STEPS.iter().position(|m| *m >= selected).unwrap_or(IDLE_STEPS.len() - 1);
    let bar = |on: bool| div().flex_1().h(px(4.)).bg(rgb(if on { BTC } else { SURFACE_3 }));
    let mut track = div().flex().w_full().h(px(22.)).items_center();
    for (i, minutes) in IDLE_STEPS.iter().copied().enumerate() {
        let mut cell = div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |d, _: &MouseDownEvent, _, cx| {
                    d.set_idle_minutes(minutes);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |d, ev: &MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left) && d.settings.idle.minutes != minutes {
                    d.set_idle_minutes(minutes);
                    cx.notify();
                }
            }));
        if i == sel {
            cell = cell
                .child(bar(true))
                .child(div().size(px(14.)).rounded_full().bg(rgb(TEXT)).border_2().border_color(rgb(BTC)))
                .child(bar(false));
        } else {
            cell = cell.child(bar(i < sel));
        }
        track = track.child(cell);
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(track)
        .child(
            div()
                .flex()
                .justify_between()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(fmt_minutes(IDLE_STEPS[0]))
                .child(fmt_minutes(IDLE_STEPS[IDLE_STEPS.len() - 1])),
        )
        .into_any_element()
}
