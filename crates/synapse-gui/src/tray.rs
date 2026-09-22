//! System tray icon (StatusNotifierItem): battery status, quick settings
//! and desktop notifications. Runs as `synapse-linux --tray`, separately
//! from the main window, so closing the window never stops it.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime};

use anyhow::anyhow;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};

use synapse_core::config::Config;
use synapse_core::eq::HardwarePreset;
use synapse_core::manager::{DeviceManager, ManagerOptions, Phase, Request, Snapshot};

use crate::i18n::Strings;
use crate::{icon, system};

enum Event {
    Changed,
    Quit,
}

#[derive(Clone, Default, PartialEq)]
struct View {
    phase: Option<Phase>,
    name: String,
    battery: Option<u8>,
    charging: bool,
    preset: Option<HardwarePreset>,
    sidetone: Option<bool>,
}

impl View {
    fn from_snapshot(snap: &Snapshot) -> Self {
        Self {
            phase: Some(snap.phase.clone()),
            name: snap.device.as_ref().map(|d| d.name.to_string()).unwrap_or_default(),
            battery: snap.state.battery,
            charging: snap.state.charging.unwrap_or(false),
            preset: snap.state.eq_preset,
            sidetone: snap.state.sidetone_enabled,
        }
    }

    fn online(&self) -> bool {
        self.phase == Some(Phase::Ready)
    }

    fn status_line(&self, t: &Strings) -> String {
        match &self.phase {
            Some(Phase::Ready) => {
                let battery = self.battery.map(|b| t.percent(b)).unwrap_or_else(|| t.unknown.into());
                let charging = if self.charging {
                    format!(" · {}", t.charging)
                } else {
                    String::new()
                };
                format!("{}: {battery}{charging}", t.tray_battery)
            }
            Some(Phase::HeadsetOffline) => t.status_offline.into(),
            Some(Phase::PermissionDenied(_)) => t.status_permission.into(),
            Some(Phase::Connecting) => t.status_connecting.into(),
            Some(Phase::NoDevice) | None => t.status_no_device.into(),
        }
    }
}

struct SynapseTray {
    manager: Arc<DeviceManager>,
    events: mpsc::Sender<Event>,
    view: View,
    strings: &'static Strings,
    simulate: bool,
}

fn open_window(simulate: bool) {
    let args: &[&str] = if simulate { &["--simulate"] } else { &[] };
    if let Err(err) = system::spawn_self(args) {
        log::error!("cannot start the main window: {err}");
    }
}

impl ksni::Tray for SynapseTray {
    fn id(&self) -> String {
        "synapse-linux".into()
    }

    fn title(&self) -> String {
        "Synapse for Linux".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 48, 64]
            .into_iter()
            .map(|size| ksni::Icon {
                width: size as i32,
                height: size as i32,
                data: icon::rgba_to_argb(&icon::tray_icon_rgba(size, self.view.online(), self.view.battery)),
            })
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let title = if self.view.name.is_empty() {
            "Synapse for Linux".to_string()
        } else {
            self.view.name.clone()
        };
        ksni::ToolTip {
            title,
            description: self.view.status_line(self.strings),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_window(self.simulate);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let t = self.strings;
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: t.tray_open.into(),
                icon_name: "audio-headset".into(),
                activate: Box::new(|tray: &mut Self| open_window(tray.simulate)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.view.status_line(t),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];

        if self.view.online() {
            let presets = HardwarePreset::SELECTABLE;
            let selected = self
                .view
                .preset
                .and_then(|p| presets.iter().position(|x| *x == p))
                .unwrap_or(usize::MAX);
            items.push(
                SubMenu {
                    label: t.tray_eq.into(),
                    submenu: vec![
                        RadioGroup {
                            selected,
                            // The menu follows the device once the change is confirmed.
                            select: Box::new(move |tray: &mut Self, index: usize| {
                                if let Some(preset) = presets.get(index) {
                                    tray.manager.request(Request::SelectPreset(*preset));
                                }
                            }),
                            options: presets
                                .iter()
                                .map(|p| RadioItem {
                                    label: t.preset(*p),
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
            items.push(
                CheckmarkItem {
                    label: t.tray_sidetone.into(),
                    checked: self.view.sidetone == Some(true),
                    activate: Box::new(|tray: &mut Self| {
                        let on = tray.view.sidetone != Some(true);
                        tray.manager.request(Request::SetSidetone(on));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: t.tray_quit.into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(Event::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn config_mtime() -> Option<SystemTime> {
    Config::default_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

pub fn run(simulate: bool) -> anyhow::Result<()> {
    let _instance = match system::single_instance("tray") {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            eprintln!("The Synapse for Linux tray is already running.");
            return Ok(());
        }
        Err(err) => {
            log::warn!("cannot check for another running tray ({err}); starting anyway");
            None
        }
    };

    let mut config = Config::load();
    let mut seen_mtime = config_mtime();
    let mut strings = Strings::get(config.language);

    let (tx, rx) = mpsc::channel();
    let notify_tx = tx.clone();
    let options = ManagerOptions {
        poll_interval: Duration::from_secs(config.tray_poll_seconds),
        simulate,
        ..ManagerOptions::default()
    };
    let manager = Arc::new(DeviceManager::start(options, move || {
        let _ = notify_tx.send(Event::Changed);
    }));

    let tray = SynapseTray {
        manager: Arc::clone(&manager),
        events: tx,
        view: View::default(),
        strings,
        simulate,
    };
    let handle = tray
        .spawn()
        .map_err(|err| anyhow!("cannot create the tray icon (no StatusNotifierItem host?): {err}"))?;

    let mut notifier = Notifier::default();
    let mut shown = View::default();
    loop {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Event::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Changed) | Err(RecvTimeoutError::Timeout) => {}
        }
        // Coalesce bursts of updates.
        let mut quit = false;
        while let Ok(event) = rx.try_recv() {
            quit |= matches!(event, Event::Quit);
        }
        if quit {
            break;
        }

        // Settings may have been changed in the main window.
        let mtime = config_mtime();
        let mut strings_changed = false;
        if mtime != seen_mtime {
            seen_mtime = mtime;
            config = Config::load();
            let new_strings = Strings::get(config.language);
            strings_changed = !std::ptr::eq(new_strings, strings);
            strings = new_strings;
        }

        let snap = manager.snapshot();
        let view = View::from_snapshot(&snap);
        if view != shown || strings_changed {
            shown = view.clone();
            handle.update(|tray| {
                tray.view = view;
                tray.strings = strings;
            });
        }
        notifier.update(&snap, &config, strings);
    }

    handle.shutdown().wait();
    Ok(())
}

/// Decides when to show desktop notifications.
#[derive(Default)]
struct Notifier {
    was_online: Option<bool>,
    low_warned: bool,
    full_notified: bool,
}

impl Notifier {
    fn update(&mut self, snap: &Snapshot, config: &Config, t: &Strings) {
        let prefs = &config.notifications;
        let online = snap.phase == Phase::Ready;
        let name = snap.device.as_ref().map(|d| d.name).unwrap_or("Razer");

        if let Some(was) = self.was_online
            && was != online
            && prefs.enabled
            && prefs.connection
        {
            let summary = if online {
                t.notif_connected
            } else {
                t.notif_disconnected
            };
            notify(summary, name, "audio-headset");
        }
        self.was_online = Some(online);
        if !online {
            return;
        }

        let Some(level) = snap.state.battery else { return };
        let charging = snap.state.charging.unwrap_or(false);
        let threshold = prefs.low_battery_percent;

        if charging || level > threshold.saturating_add(5) {
            self.low_warned = false;
        }
        if prefs.enabled && threshold > 0 && !charging && level <= threshold && !self.low_warned {
            notify(
                t.notif_low_title,
                &format!("{name} · {}", t.percent(level)),
                "battery-caution",
            );
            self.low_warned = true;
        }

        if charging && level >= 100 {
            if prefs.enabled && prefs.full_charge && !self.full_notified {
                notify(t.notif_full_title, t.notif_full_body, "battery-full-charged");
            }
            self.full_notified = true;
        } else if !charging {
            self.full_notified = false;
        }
    }
}

fn notify(summary: &str, body: &str, icon: &str) {
    let result = notify_rust::Notification::new()
        .appname("Synapse for Linux")
        .summary(summary)
        .body(body)
        .icon(icon)
        .timeout(notify_rust::Timeout::Milliseconds(8000))
        .show();
    if let Err(err) = result {
        log::warn!("cannot show notification: {err}");
    }
}
