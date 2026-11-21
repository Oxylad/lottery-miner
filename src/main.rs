#![cfg_attr(not(test), windows_subsystem = "windows")]
//! Lottery Miner: tray app that runs the local cpuminer-opt instances and
//! watches/controls the home server's miner over SSH.

mod app;
mod input;
mod job;
mod logbook;
mod metrics;
mod miner;
mod network;
mod odds;
mod parse;
mod remote;
mod settings;
mod ui;
mod win;

use std::time::{Duration, Instant};

use gpui::{
    App, Application, AsyncApp, Bounds, Entity, TitlebarOptions, WindowBounds, WindowOptions,
    prelude::*, px, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::{
    MouseButton as TrayButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use app::Dashboard;
use remote::ServiceAction;

/// Orange disc on transparent background, drawn in code (no asset files).
/// The logo from the exe's icon resource (#1, see assets/app.rc) at the
/// system's small-icon size; the drawn disc below only if that fails.
fn app_tray_icon() -> tray_icon::Icon {
    let size = win::small_icon_size();
    tray_icon::Icon::from_resource(1, Some((size, size))).unwrap_or_else(|_| tray_icon_image())
}

fn tray_icon_image() -> tray_icon::Icon {
    let n = 32u32;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let inner = d < 9.0 && d > 5.0;
            if inner {
                rgba.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            } else if d <= 15.0 {
                rgba.extend_from_slice(&[0xF7, 0x93, 0x1A, 0xFF]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, n, n).expect("tray icon image")
}

struct Tray {
    icon: TrayIcon,
    open: MenuItem,
    mining: MenuItem,
    gpu: MenuItem,
    oxy_start: MenuItem,
    oxy_stop: MenuItem,
    quit: MenuItem,
}

fn build_tray() -> Result<Tray, String> {
    let menu = Menu::new();
    let open = MenuItem::new("Open", true, None);
    let mining = MenuItem::new("Start CPU mining", true, None);
    let gpu = MenuItem::new("Start GPU mining", true, None);
    let oxy_start = MenuItem::new("oxyhome: Start miner", true, None);
    let oxy_stop = MenuItem::new("oxyhome: Stop miner", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &open,
        &PredefinedMenuItem::separator(),
        &mining,
        &gpu,
        &PredefinedMenuItem::separator(),
        &oxy_start,
        &oxy_stop,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| e.to_string())?;
    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Lottery Miner")
        .with_icon(app_tray_icon())
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Tray {
        icon,
        open,
        mining,
        gpu,
        oxy_start,
        oxy_stop,
        quit,
    })
}

/// Drains tray and menu events on the GPUI main thread, and keeps the
/// tooltip and the mining menu label current.
fn spawn_tray_loop(tray: Tray, dashboard: Entity<Dashboard>, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_label = Instant::now() - Duration::from_secs(10);
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let mut quit = false;
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                let id = ev.id;
                let _ = cx.update(|cx| {
                    dashboard.update(cx, |d, cx| {
                        if id == *tray.open.id() {
                            d.show_window();
                        } else if id == *tray.mining.id() {
                            d.toggle_mining();
                        } else if id == *tray.gpu.id() {
                            d.toggle_gpu();
                        } else if id == *tray.oxy_start.id() {
                            d.oxyhome_control(ServiceAction::Start, cx);
                        } else if id == *tray.oxy_stop.id() {
                            d.oxyhome_control(ServiceAction::Stop, cx);
                        } else if id == *tray.quit.id() {
                            quit = true;
                        }
                        cx.notify();
                    })
                });
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: TrayButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    let _ = cx.update(|cx| dashboard.read(cx).show_window());
                }
            }
            if quit {
                let _ = cx.update(|cx| {
                    dashboard.update(cx, |d, _| {
                        d.delete_metrics();
                        if d.settings.stop_miners_on_quit {
                            d.stop_local_miners();
                        } else {
                            d.miner.detach();
                            d.gpu_miner.detach();
                            crate::job::kill_miners_on_exit(false);
                        }
                    });
                    cx.quit();
                });
                break;
            }
            if last_label.elapsed() >= Duration::from_secs(2) {
                last_label = Instant::now();
                let state = cx.update(|cx| {
                    let d = dashboard.read(cx);
                    (d.miner.is_running(), d.gpu_miner.is_running(), d.total_hashrate(), d.oxyhome_hashrate())
                });
                if let Ok((running, gpu_running, total, oxy)) = state {
                    tray.mining.set_text(if running {
                        "Stop CPU mining"
                    } else {
                        "Start CPU mining"
                    });
                    tray.gpu.set_text(if gpu_running {
                        "Stop GPU mining"
                    } else {
                        "Start GPU mining"
                    });
                    let mut tip = if running || gpu_running {
                        format!("Lottery Miner: {}", odds::format_hashrate(total))
                    } else {
                        "Lottery Miner: stopped".to_string()
                    };
                    if let Some(o) = oxy {
                        tip.push_str(&format!(" + oxyhome {}", odds::format_hashrate(o)));
                    }
                    let _ = tray.icon.set_tooltip(Some(tip));
                }
            }
        }
    })
    .detach();
}

fn main() {
    let start_hidden = std::env::args().any(|a| a == "--hidden");
    // There is no console: keep panics in the log file.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logbook::file::note(&format!("app: PANIC {info}"));
        default_hook(info);
    }));
    logbook::file::note(&format!(
        "app {} starting{}{}",
        env!("CARGO_PKG_VERSION"),
        if start_hidden { ", hidden" } else { "" },
        if win::is_elevated() { ", as administrator" } else { "" },
    ));
    Application::new().run(move |cx: &mut App| {
        logbook::file::note("app: window system ready");
        input::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(980.), px(760.)), cx);
        let mut dashboard: Option<Entity<Dashboard>> = None;
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Lottery Miner".into()),
                    ..Default::default()
                }),
                show: !start_hidden,
                focus: !start_hidden,
                ..Default::default()
            },
            |window, cx| {
                let hwnd = match window.window_handle().map(|h| h.as_raw()) {
                    Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
                    _ => None,
                };
                // Closing the window hides it to the tray; Quit is in the tray menu.
                window.on_window_should_close(cx, move |_, _| {
                    if let Some(h) = hwnd {
                        win::hide_window(h);
                    }
                    false
                });
                let entity = cx.new(|cx| {
                    let mut d = Dashboard::new(cx);
                    d.hwnd = hwnd;
                    d
                });
                dashboard = Some(entity.clone());
                entity
            },
        );
        logbook::file::note(&format!("app: main window {}", if opened.is_ok() { "open" } else { "failed" }));
        if let Err(e) = &opened {
            logbook::file::note(&format!("app: could not open the main window: {e}"));
        }
        let (Ok(_), Some(dashboard)) = (opened, dashboard) else {
            eprintln!("could not open the main window");
            cx.quit();
            return;
        };
        if start_hidden {
            if let Some(h) = dashboard.read(cx).hwnd {
                win::hide_window(h);
            }
        }
        match build_tray() {
            Ok(tray) => spawn_tray_loop(tray, dashboard, cx),
            Err(e) => {
                // Without a tray the hidden window could never come back; show it.
                dashboard.update(cx, |d, _| {
                    d.message = Some(format!("tray icon failed: {e}"));
                    d.show_window();
                });
            }
        }
    });
}
