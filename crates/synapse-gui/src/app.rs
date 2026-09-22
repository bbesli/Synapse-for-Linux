//! Main window.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Label, Layout, Margin, Order, Rect, RichText,
    Sense, Stroke, TextureHandle, Ui, Vec2,
};

use synapse_core::audio::{self, MicCleanStatus};
use synapse_core::config::{Config, Language};
use synapse_core::devices::Connection;
use synapse_core::devices::blackshark_v2_hs::protocol::SIDETONE_MAX;
use synapse_core::devices::blackshark_v2_hs::{HeadsetState, LedMode};
use synapse_core::eq::{self, Bands, HardwarePreset};
use synapse_core::manager::{DeviceManager, ManagerOptions, Phase, Problem, Request, Snapshot, format_clock};

use crate::eq_graph::eq_graph;
use crate::i18n::Strings;
use crate::theme::{
    self, ACCENT, ACCENT_BG, BG, CARD, CARD_BORDER, CONTROL, DANGER, PANEL, TEXT, TEXT_DIM, TEXT_FAINT, WARNING,
};
use crate::widgets::{self, Glyph, card, card_title, chip, section_label, setting_row, toggle};
use crate::{icon, system};

pub const UDEV_RULE: &str = include_str!("../../../packaging/udev/70-synapse-linux.rules");
const UDEV_TARGET: &str = "/etc/udev/rules.d/70-synapse-linux.rules";
const SLEEP_CHOICES: [u8; 9] = [0, 5, 10, 15, 30, 45, 60, 90, 120];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Audio,
    Mic,
    Power,
    Device,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Success,
    Error,
    Info,
}

struct Toast {
    text: String,
    kind: ToastKind,
    until: Instant,
}

enum TaskResult {
    MicStatus(MicCleanStatus),
    MicChanged(Result<(), String>, MicCleanStatus),
    MicDefault(Result<(), String>),
    Permission(Result<(), String>),
}

pub struct SynapseApp {
    manager: DeviceManager,
    config: Config,
    simulate: bool,
    page: Page,
    snap: Snapshot,
    logo: TextureHandle,

    /// Requests sent but not yet confirmed; shown optimistically.
    overlay: Vec<Request>,
    writes_pending: bool,
    seen_problem: u64,
    toasts: Vec<Toast>,

    eq_edit: Option<Bands>,
    eq_apply_at: Option<Instant>,
    auto_apply: bool,
    curve_name: String,
    sidetone_drag: Option<u8>,
    sleep_custom: u8,

    mic: Option<MicCleanStatus>,
    mic_checked: Option<Instant>,
    tasks: Vec<Receiver<TaskResult>>,
    mic_busy: bool,
    permission_busy: bool,

    autostart: bool,
    tray_running: bool,
    tray_checked: Option<Instant>,
}

impl SynapseApp {
    pub fn new(cc: &eframe::CreationContext<'_>, simulate: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let manager = DeviceManager::start(
            ManagerOptions {
                simulate,
                ..ManagerOptions::default()
            },
            move || ctx.request_repaint(),
        );
        let logo_size = 64;
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([logo_size, logo_size], &icon::app_icon_rgba(logo_size as u32)),
            egui::TextureOptions::LINEAR,
        );
        Self {
            manager,
            config: Config::load(),
            simulate,
            page: Page::Audio,
            snap: Snapshot::default(),
            logo,
            overlay: Vec::new(),
            writes_pending: false,
            seen_problem: 0,
            toasts: Vec::new(),
            eq_edit: None,
            eq_apply_at: None,
            auto_apply: true,
            curve_name: String::new(),
            sidetone_drag: None,
            sleep_custom: 20,
            mic: None,
            mic_checked: None,
            tasks: Vec::new(),
            mic_busy: false,
            permission_busy: false,
            autostart: system::autostart_enabled(),
            tray_running: system::instance_running("tray"),
            tray_checked: Some(Instant::now()),
        }
    }

    fn strings(&self) -> &'static Strings {
        Strings::get(self.config.language)
    }

    #[cfg(test)]
    pub(crate) fn set_page_for_tests(&mut self, page: Page) {
        self.page = page;
    }

    #[cfg(test)]
    pub(crate) fn set_language_for_tests(&mut self, language: Language) {
        self.config.language = language;
    }

    #[cfg(test)]
    pub(crate) fn is_ready_for_tests(&self) -> bool {
        self.snap.is_ready()
    }

    // ----- state plumbing ---------------------------------------------------

    fn sync(&mut self, ctx: &egui::Context) {
        let t = self.strings();
        let mut snap = self.manager.snapshot();
        let now = Instant::now();

        if let Some((id, problem)) = snap.problem.clone()
            && id != self.seen_problem
        {
            self.seen_problem = id;
            let text = match problem {
                Problem::NotConnected => t.problem_not_connected.to_string(),
                Problem::HeadsetOffline => t.problem_offline.to_string(),
                Problem::Timeout => t.problem_timeout.to_string(),
                Problem::Busy => t.problem_busy.to_string(),
                Problem::Other(msg) => format!("{}: {msg}", t.problem_other),
            };
            self.toast(text, ToastKind::Error);
            // Keep the optimistic values of requests still running; once
            // nothing is pending the device state below is authoritative.
            self.writes_pending = false;
        }
        if snap.pending == 0 {
            self.overlay.clear();
            if self.writes_pending {
                self.writes_pending = false;
                self.toast(t.saved_to_headset.into(), ToastKind::Success);
            }
        }
        for request in &self.overlay {
            optimistic(&mut snap.state, request);
        }
        self.snap = snap;

        if let Some(at) = self.eq_apply_at {
            if now >= at {
                self.eq_apply_at = None;
                if let Some(bands) = self.eq_edit {
                    self.apply_curve(bands);
                }
            } else {
                ctx.request_repaint_after(at - now);
            }
        }

        self.poll_tasks(ctx);

        if self.page == Page::Mic
            && !self.mic_busy
            && self.mic_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(8))
        {
            self.mic_checked = Some(now);
            self.spawn_task(ctx, || TaskResult::MicStatus(audio::status()));
        }
        if self.page == Page::Settings && self.tray_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.tray_checked = Some(now);
            self.tray_running = system::instance_running("tray");
            self.autostart = system::autostart_enabled();
            ctx.request_repaint_after(Duration::from_secs(2));
        }

        self.toasts.retain(|toast| toast.until > now);
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn spawn_task(&mut self, ctx: &egui::Context, job: impl FnOnce() -> TaskResult + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
        self.tasks.push(rx);
    }

    fn poll_tasks(&mut self, ctx: &egui::Context) {
        let t = self.strings();
        let mut results = Vec::new();
        self.tasks.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                results.push(result);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        if !self.tasks.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        for result in results {
            match result {
                TaskResult::MicStatus(status) => self.mic = Some(status),
                TaskResult::MicChanged(outcome, status) => {
                    self.mic_busy = false;
                    self.mic = Some(status);
                    if let Err(err) = outcome {
                        self.toast(format!("{}: {err}", t.problem_other), ToastKind::Error);
                    }
                }
                TaskResult::MicDefault(outcome) => {
                    self.mic_busy = false;
                    match outcome {
                        Ok(()) => {
                            if let Some(mic) = &mut self.mic {
                                mic.is_default = true;
                            }
                        }
                        Err(err) => self.toast(format!("{}: {err}", t.problem_other), ToastKind::Error),
                    }
                }
                TaskResult::Permission(outcome) => {
                    self.permission_busy = false;
                    match outcome {
                        Ok(()) => self.manager.request(Request::Refresh),
                        Err(err) => self.toast(format!("{}: {err}", t.problem_other), ToastKind::Error),
                    }
                }
            }
        }
    }

    fn toast(&mut self, text: String, kind: ToastKind) {
        let secs = match kind {
            ToastKind::Error => 6,
            ToastKind::Success => 2,
            ToastKind::Info => 3,
        };
        self.toasts.retain(|t| t.text != text);
        self.toasts.push(Toast {
            text,
            kind,
            until: Instant::now() + Duration::from_secs(secs),
        });
    }

    /// Send a request to the device and show its effect right away.
    fn act(&mut self, request: Request) {
        optimistic(&mut self.snap.state, &request);
        self.overlay.push(request.clone());
        self.writes_pending = true;
        self.manager.request(request);
    }

    fn apply_curve(&mut self, bands: Bands) {
        self.eq_edit = None;
        self.eq_apply_at = None;
        self.act(Request::ApplyCustomEq(bands));
        self.config.last_custom_curve = Some(bands);
        self.save_config();
    }

    fn save_config(&mut self) {
        if let Err(err) = self.config.save() {
            log::warn!("cannot save config: {err}");
            let t = self.strings();
            self.toast(format!("{}: {err}", t.config_save_failed), ToastKind::Error);
        }
    }

    fn online(&self) -> bool {
        self.snap.phase == Phase::Ready
    }

    // ----- chrome -----------------------------------------------------------

    fn header(&mut self, ui: &mut Ui, t: &Strings) {
        ui.horizontal_centered(|ui| {
            ui.add(egui::Image::new((self.logo.id(), Vec2::splat(30.0))));
            ui.add_space(2.0);
            ui.label(RichText::new("SYNAPSE").font(theme::heading(19.0)).color(ACCENT));
            ui.label(RichText::new("for Linux").color(TEXT_DIM).size(13.0));
            ui.add_space(18.0);

            if let Some(device) = &self.snap.device {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 30.0), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, CARD_BORDER);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(device.name).color(TEXT).size(14.5).strong());
                    ui.horizontal(|ui| {
                        let (color, text) = phase_badge(&self.snap.phase, t);
                        widgets::status_dot(ui, color);
                        ui.label(RichText::new(text).color(TEXT_DIM).size(12.0));
                        let connection = match device.connection {
                            Connection::Dongle => t.conn_dongle,
                            Connection::Wired => t.conn_wired,
                        };
                        ui.label(RichText::new(format!("· {connection}")).color(TEXT_FAINT).size(12.0));
                        if device.simulated {
                            ui.label(RichText::new(format!("· {}", t.simulated)).color(WARNING).size(12.0));
                        }
                    });
                });
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.online() {
                    let state = &self.snap.state;
                    if let Some(level) = state.battery {
                        ui.label(RichText::new(t.percent(level)).color(TEXT).size(15.0).strong());
                    }
                    let charging = state.charging == Some(true);
                    widgets::battery_gauge(ui, state.battery, charging, Vec2::new(34.0, 17.0));
                }
                if self.snap.pending > 0 || self.mic_busy || self.permission_busy {
                    ui.add_space(8.0);
                    ui.add(egui::Spinner::new().size(16.0).color(ACCENT));
                }
            });
        });
    }

    fn nav(&mut self, ui: &mut Ui, t: &Strings) {
        let pages = [
            (Page::Audio, Glyph::Headphones, t.nav_audio),
            (Page::Mic, Glyph::Microphone, t.nav_mic),
            (Page::Power, Glyph::Battery, t.nav_power),
            (Page::Device, Glyph::Info, t.nav_device),
            (Page::Settings, Glyph::Gear, t.nav_settings),
        ];
        for (page, glyph, label) in pages {
            let selected = self.page == page;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 42.0), Sense::click());
            if response.clicked() {
                self.page = page;
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let painter = ui.painter();
            if selected {
                painter.rect_filled(rect, CornerRadius::same(8), ACCENT_BG);
                let bar = Rect::from_min_size(rect.min + Vec2::new(0.0, 10.0), Vec2::new(3.0, 22.0));
                painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
            } else if response.hovered() {
                painter.rect_filled(rect, CornerRadius::same(8), CONTROL);
            }
            let color = if selected { ACCENT } else { TEXT_DIM };
            widgets::paint_glyph(painter, rect.left_center() + Vec2::new(25.0, 0.0), 18.0, glyph, color);
            painter.text(
                rect.left_center() + Vec2::new(44.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(14.5),
                if selected { TEXT } else { TEXT_DIM },
            );
            ui.add_space(2.0);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .color(TEXT_FAINT)
                    .size(11.5),
            );
            if self.simulate {
                ui.label(RichText::new(t.simulated).color(WARNING).size(11.5));
            }
        });
    }

    /// Status card shown above device pages. Returns whether the page itself
    /// should be drawn.
    fn device_banner(&mut self, ui: &mut Ui, t: &Strings) -> bool {
        match self.snap.phase.clone() {
            Phase::Ready => true,
            Phase::HeadsetOffline => {
                banner(ui, WARNING, "⚠", t.status_offline, Some(t.status_offline_hint));
                true
            }
            Phase::Connecting => {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(20.0).color(ACCENT));
                        ui.label(RichText::new(t.status_connecting).color(TEXT).size(15.0));
                    });
                });
                false
            }
            Phase::NoDevice => {
                card(ui, |ui| {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        widgets::glyph(ui, Glyph::Headphones, 54.0, TEXT_FAINT);
                        ui.add_space(6.0);
                        ui.label(RichText::new(t.status_no_device).font(theme::heading(19.0)).color(TEXT));
                        ui.add(Label::new(RichText::new(t.status_no_device_hint).color(TEXT_DIM)).wrap());
                        ui.add_space(10.0);
                        ui.add(egui::Spinner::new().size(18.0).color(TEXT_FAINT));
                    });
                    ui.add_space(24.0);
                });
                false
            }
            Phase::PermissionDenied(path) => {
                self.permission_card(ui, t, &path);
                false
            }
        }
    }

    fn permission_card(&mut self, ui: &mut Ui, t: &Strings, path: &str) {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "synapse-linux".into());
        let command = format!(
            "\"{exe}\" --print-udev-rule | sudo tee {UDEV_TARGET} >/dev/null && sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=hidraw"
        );
        let ctx = ui.ctx().clone();
        card(ui, |ui| {
            ui.label(
                RichText::new(t.status_permission)
                    .font(theme::heading(18.0))
                    .color(TEXT),
            );
            ui.label(RichText::new(path).color(TEXT_FAINT).monospace().size(12.0));
            ui.add(Label::new(RichText::new(t.status_permission_hint).color(TEXT_DIM)).wrap());
            ui.add_space(4.0);
            Frame::new()
                .fill(BG)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        Label::new(RichText::new(&command).monospace().color(TEXT).size(12.5))
                            .wrap()
                            .selectable(true),
                    );
                });
            ui.horizontal(|ui| {
                let grant = ui.add_enabled(
                    !self.permission_busy,
                    egui::Button::new(RichText::new(t.grant_access).color(BG).strong())
                        .fill(ACCENT)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(0.0, 30.0)),
                );
                if grant.on_hover_text(t.grant_access_hint).clicked() {
                    self.permission_busy = true;
                    self.spawn_task(&ctx, || TaskResult::Permission(install_udev_rule_with_pkexec()));
                }
                if widgets::secondary_button(ui, t.copy_command).clicked() {
                    ui.ctx().copy_text(command.clone());
                    self.toast(t.copied.into(), ToastKind::Info);
                }
            });
        });
    }

    // ----- pages ------------------------------------------------------------

    fn page_audio(&mut self, ui: &mut Ui, t: &Strings) {
        let online = self.online();
        let state = self.snap.state.clone();
        let custom_slot = state.custom_eq.unwrap_or(eq::FLAT);
        // The live curve: the Custom slot, or the (approximate) factory curve of a ROM preset.
        let live_bands = match state.eq_preset {
            Some(preset) if preset != HardwarePreset::Custom => preset.reference_curve().unwrap_or(custom_slot),
            _ => custom_slot,
        };
        let rom_active = state.eq_preset.is_some_and(|p| p.is_rom());
        let displayed = self.eq_edit.unwrap_or(live_bands);
        let custom_active = state.eq_preset == Some(HardwarePreset::Custom) && self.eq_edit.is_none();

        card(ui, |ui| {
            card_title(ui, t.eq_title, Some(t.eq_subtitle));
            ui.add_enabled_ui(online, |ui| {
                section_label(ui, t.hw_presets);
                ui.horizontal_wrapped(|ui| {
                    for preset in HardwarePreset::SELECTABLE {
                        let selected = state.eq_preset == Some(preset) && self.eq_edit.is_none();
                        if chip(ui, &t.preset(preset), selected).clicked() {
                            self.eq_edit = None;
                            self.eq_apply_at = None;
                            self.act(Request::SelectPreset(preset));
                        }
                    }
                });
                ui.add_space(2.0);
                section_label(ui, t.curves);
                ui.horizontal_wrapped(|ui| {
                    for curve in eq::BUILTIN_CURVES {
                        let selected = custom_active && custom_slot == curve.bands;
                        if chip(ui, t.curve_name(curve), selected).clicked() {
                            self.apply_curve(curve.bands);
                        }
                    }
                });
                if !self.config.user_presets.is_empty() {
                    ui.add_space(2.0);
                    section_label(ui, t.my_curves);
                    let mut remove = None;
                    let mut apply = None;
                    ui.horizontal_wrapped(|ui| {
                        for preset in &self.config.user_presets {
                            let selected = custom_active && custom_slot == preset.bands;
                            let response = chip(ui, &preset.name, selected);
                            if response.clicked() {
                                apply = Some(preset.bands);
                            }
                            response.context_menu(|ui| {
                                if ui.button(format!("🗑  {}", t.delete)).clicked() {
                                    remove = Some(preset.name.clone());
                                    ui.close();
                                }
                            });
                        }
                    });
                    if let Some(bands) = apply {
                        self.apply_curve(bands);
                    }
                    if let Some(name) = remove {
                        self.config.remove_preset(&name);
                        self.save_config();
                    }
                }
            });

            ui.add_space(6.0);
            let mut bands = displayed;
            let ghost = self.eq_edit.is_some().then_some(live_bands);
            let out = eq_graph(ui, "main", &mut bands, ghost.as_ref(), online);
            if out.changed {
                self.eq_edit = Some(bands);
            }
            if self.auto_apply && self.eq_edit.is_some() {
                if out.scrolled {
                    self.eq_apply_at = Some(Instant::now() + Duration::from_millis(700));
                } else if out.committed {
                    self.apply_curve(bands);
                }
            }
            let hint = if rom_active && self.eq_edit.is_none() {
                format!("{}  ·  {}", t.factory_curve_note, t.eq_edit_hint)
            } else {
                t.eq_edit_hint.to_string()
            };
            ui.add(Label::new(RichText::new(hint).color(TEXT_FAINT).size(12.0)).wrap());

            ui.add_enabled_ui(online, |ui| {
                ui.horizontal(|ui| {
                    toggle(ui, &mut self.auto_apply);
                    ui.label(RichText::new(t.auto_apply).color(TEXT_DIM));
                    if self.eq_edit.is_some() {
                        ui.add_space(8.0);
                        ui.label(RichText::new(format!("● {}", t.not_applied)).color(WARNING).size(12.5));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let edit = self.eq_edit;
                        if ui
                            .add_enabled(
                                edit.is_some(),
                                egui::Button::new(RichText::new(t.apply).color(BG).strong()).fill(ACCENT),
                            )
                            .clicked()
                            && let Some(bands) = edit
                        {
                            self.apply_curve(bands);
                        }
                        if ui.add_enabled(edit.is_some(), egui::Button::new(t.revert)).clicked() {
                            self.eq_edit = None;
                            self.eq_apply_at = None;
                        }
                        if ui.button(t.reset_flat).clicked() {
                            if self.auto_apply {
                                self.apply_curve(eq::FLAT);
                            } else {
                                self.eq_edit = Some(eq::FLAT);
                            }
                        }
                    });
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new(t.save_as).color(TEXT_DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut self.curve_name)
                        .hint_text(t.curve_name_hint)
                        .desired_width(200.0)
                        .char_limit(40),
                );
                let name = self.curve_name.trim().to_string();
                if ui.add_enabled(!name.is_empty(), egui::Button::new(t.save)).clicked()
                    && self.config.upsert_preset(&name, displayed)
                {
                    self.curve_name.clear();
                    self.save_config();
                    self.toast(format!("✓ {name}"), ToastKind::Success);
                }
            });
        });

        card(ui, |ui| {
            card_title(ui, t.eq_advanced, None);
            ui.add_enabled_ui(online, |ui| {
                let mut on = state.eq_enabled.unwrap_or(true);
                if setting_row(ui, t.eq_stage, Some(t.eq_stage_desc), 60.0, |ui| toggle(ui, &mut on)).changed() {
                    self.act(Request::SetEqEnabled(on));
                }
                ui.separator();
                let mut on = state.enhancement.unwrap_or(false);
                if setting_row(ui, t.enhancement, Some(t.enhancement_desc), 60.0, |ui| {
                    toggle(ui, &mut on)
                })
                .changed()
                {
                    self.act(Request::SetEnhancement(on));
                }
            });
        });
    }

    fn page_mic(&mut self, ui: &mut Ui, t: &Strings) {
        let online = self.online();
        let state = self.snap.state.clone();
        let ctx = ui.ctx().clone();

        card(ui, |ui| {
            ui.add_enabled_ui(online, |ui| {
                let mut on = state.sidetone_enabled.unwrap_or(false);
                if setting_row(ui, t.sidetone_title, Some(t.sidetone_desc), 60.0, |ui| {
                    toggle(ui, &mut on)
                })
                .changed()
                {
                    self.act(Request::SetSidetone(on));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t.level).color(TEXT_DIM));
                    let mut level = self.sidetone_drag.or(state.sidetone_level).unwrap_or(0);
                    let response = ui.add(egui::Slider::new(&mut level, 0..=SIDETONE_MAX).show_value(true));
                    if response.dragged() {
                        self.sidetone_drag = Some(level);
                    }
                    if response.drag_stopped() || (response.changed() && !response.dragged()) {
                        self.sidetone_drag = None;
                        self.act(Request::SetSidetoneLevel(level));
                        if state.sidetone_enabled == Some(false) && level > 0 {
                            self.act(Request::SetSidetone(true));
                        }
                    }
                });
            });
        });

        card(ui, |ui| {
            card_title(ui, t.mic_status, None);
            let (icon, text, color) = match state.mic_muted.filter(|_| online) {
                Some(true) => (Glyph::MicrophoneMuted, t.mic_muted, DANGER),
                Some(false) => (Glyph::Microphone, t.mic_live, ACCENT),
                None => (Glyph::Microphone, t.unknown, TEXT_DIM),
            };
            ui.horizontal(|ui| {
                widgets::glyph(ui, icon, 24.0, color);
                ui.label(RichText::new(text).size(15.0).color(color));
            });
            ui.add(Label::new(RichText::new(t.mic_hint).color(TEXT_FAINT).size(12.0)).wrap());
        });

        card(ui, |ui| {
            card_title(ui, t.mic_enhance_title, Some(t.mic_enhance_desc));
            match self.mic.clone() {
                None => {
                    ui.add(egui::Spinner::new().color(TEXT_FAINT));
                }
                Some(status) if !status.plugin_available => {
                    ui.label(RichText::new(t.mic_enhance_unavailable).color(WARNING));
                }
                Some(status) => {
                    let mut on = status.active || status.enabled;
                    let busy = self.mic_busy;
                    let changed = ui
                        .add_enabled_ui(!busy, |ui| {
                            setting_row(ui, t.mic_enhance_toggle, None, 60.0, |ui| toggle(ui, &mut on)).changed()
                        })
                        .inner;
                    if changed {
                        self.mic_busy = true;
                        let description = match t.lang {
                            Language::Tr => "Razer Mikrofon (Temiz)",
                            _ => "Razer Mic (Clean)",
                        };
                        self.spawn_task(&ctx, move || {
                            let outcome = if on {
                                match audio::find_razer_source() {
                                    Some(source) => audio::enable(&source.name, description),
                                    None => Err("Razer microphone not found in PipeWire".into()),
                                }
                            } else {
                                audio::disable()
                            };
                            // Give PipeWire a moment to create/remove the node.
                            std::thread::sleep(Duration::from_millis(600));
                            TaskResult::MicChanged(outcome, audio::status())
                        });
                    }
                    if status.active {
                        ui.horizontal(|ui| {
                            widgets::status_dot(ui, ACCENT);
                            ui.add(Label::new(RichText::new(t.mic_enhance_active).color(TEXT_DIM).size(12.5)).wrap());
                        });
                        if !status.is_default
                            && ui
                                .add_enabled_ui(!busy, |ui| widgets::secondary_button(ui, t.mic_enhance_default))
                                .inner
                                .clicked()
                        {
                            self.mic_busy = true;
                            self.spawn_task(&ctx, || {
                                TaskResult::MicDefault(audio::set_default_source(audio::CLEAN_NODE))
                            });
                        }
                    }
                }
            }
        });
    }

    fn page_power(&mut self, ui: &mut Ui, t: &Strings) {
        let online = self.online();
        let state = self.snap.state.clone();
        let dongle_present = matches!(self.snap.phase, Phase::Ready | Phase::HeadsetOffline)
            && self
                .snap
                .device
                .as_ref()
                .is_some_and(|d| d.connection == Connection::Dongle);

        card(ui, |ui| {
            card_title(ui, t.battery_title, None);
            ui.horizontal(|ui| {
                let charging = state.charging == Some(true);
                widgets::battery_gauge(
                    ui,
                    state.battery.filter(|_| online),
                    charging && online,
                    Vec2::new(92.0, 42.0),
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    let level = state
                        .battery
                        .filter(|_| online)
                        .map(|b| t.percent(b))
                        .unwrap_or_else(|| t.unknown.into());
                    ui.label(RichText::new(level).font(theme::heading(28.0)).color(TEXT));
                    let sub = match (online, charging) {
                        (false, _) => t.status_offline,
                        (true, true) => t.charging,
                        (true, false) => t.on_battery,
                    };
                    ui.label(RichText::new(sub).color(TEXT_DIM));
                });
            });
            ui.separator();
            let mut threshold = self.config.notifications.low_battery_percent;
            let response = setting_row(ui, t.low_battery_at, None, 300.0, |ui| {
                widgets::ltr_box(ui, 300.0, |ui| ui.add(t.percent_slider(&mut threshold, 0..=50)))
            });
            if response.changed() {
                self.config.notifications.low_battery_percent = threshold;
            }
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                self.save_config();
            }
        });

        card(ui, |ui| {
            card_title(ui, t.sleep_title, Some(t.sleep_desc));
            ui.add_enabled_ui(online, |ui| {
                let current = state.sleep_minutes;
                ui.horizontal_wrapped(|ui| {
                    for minutes in SLEEP_CHOICES {
                        if chip(ui, &t.minutes(minutes), current == Some(minutes)).clicked() {
                            self.act(Request::SetSleepMinutes(minutes));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t.custom_minutes).color(TEXT_DIM));
                    ui.add(egui::DragValue::new(&mut self.sleep_custom).range(1..=255).speed(0.5));
                    if widgets::secondary_button(ui, t.apply).clicked() {
                        self.act(Request::SetSleepMinutes(self.sleep_custom));
                    }
                    if let Some(minutes) = current.filter(|m| !SLEEP_CHOICES.contains(m)) {
                        ui.label(RichText::new(format!("● {}", t.minutes(minutes))).color(ACCENT));
                    }
                });
            });
        });

        if self
            .snap
            .device
            .as_ref()
            .is_none_or(|d| d.connection == Connection::Dongle)
        {
            card(ui, |ui| {
                card_title(ui, t.led_title, Some(t.led_desc));
                ui.add_enabled_ui(dongle_present, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for mode in LedMode::ALL {
                            if chip(ui, &t.led(mode), state.led_mode == Some(mode)).clicked() {
                                self.act(Request::SetLedMode(mode));
                            }
                        }
                    });
                });
            });
        }

        card(ui, |ui| {
            ui.add_enabled_ui(online, |ui| {
                let mut on = state.bt_dnd.unwrap_or(false);
                if setting_row(ui, t.dnd_title, Some(t.dnd_desc), 60.0, |ui| toggle(ui, &mut on)).changed() {
                    self.act(Request::SetBtDnd(on));
                }
            });
        });
    }

    fn page_device(&mut self, ui: &mut Ui, t: &Strings) {
        let state = self.snap.state.clone();
        let device = self.snap.device.clone();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                card_title(ui, t.device_title, None);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::secondary_button(ui, &format!("⟳  {}", t.refresh)).clicked() {
                        self.manager.request(Request::Refresh);
                    }
                });
            });
            let text = |v: &Option<String>| v.clone().unwrap_or_else(|| t.unknown.into());
            egui::Grid::new("device_info")
                .num_columns(2)
                .spacing([28.0, 10.0])
                .show(ui, |ui| {
                    if let Some(device) = &device {
                        let mut model = device.name.to_string();
                        if device.experimental {
                            model.push_str(&format!(" ({})", t.experimental));
                        }
                        widgets::info_row(ui, t.model, &model);
                        let connection = match device.connection {
                            Connection::Dongle => t.conn_dongle,
                            Connection::Wired => t.conn_wired,
                        };
                        widgets::info_row(ui, t.connection, connection);
                        widgets::info_row(ui, t.usb_id, &format!("{:04x}:{:04x}", device.vid, device.pid));
                        widgets::info_row(ui, t.device_node, &device.path);
                    }
                    widgets::info_row(ui, t.headset_fw, &text(&state.headset_firmware));
                    widgets::info_row(ui, t.headset_serial, &text(&state.headset_serial));
                    if device.as_ref().is_none_or(|d| d.connection == Connection::Dongle) {
                        widgets::info_row(ui, t.dongle_fw, &text(&state.dongle_firmware));
                        widgets::info_row(ui, t.dongle_serial, &text(&state.dongle_serial));
                    }
                });
        });

        card(ui, |ui| {
            card_title(ui, t.log_title, None);
            Frame::new()
                .fill(BG)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .stick_to_bottom(true)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for line in &self.snap.log {
                                ui.label(
                                    RichText::new(format!("{}  {}", format_clock(line.time), line.text))
                                        .monospace()
                                        .size(12.0)
                                        .color(TEXT_DIM),
                                );
                            }
                        });
                });
        });
    }

    fn page_settings(&mut self, ui: &mut Ui, t: &Strings) {
        card(ui, |ui| {
            card_title(ui, t.language, None);
            ui.horizontal(|ui| {
                for (lang, label) in [
                    (Language::Auto, t.lang_system),
                    (Language::Tr, "Türkçe"),
                    (Language::En, "English"),
                ] {
                    if chip(ui, label, self.config.language == lang).clicked() && self.config.language != lang {
                        self.config.language = lang;
                        self.save_config();
                    }
                }
            });
        });

        card(ui, |ui| {
            card_title(ui, t.tray_title, Some(t.tray_desc));
            let mut autostart = self.autostart;
            if setting_row(ui, t.tray_autostart, None, 60.0, |ui| toggle(ui, &mut autostart)).changed() {
                match system::set_autostart(autostart, self.simulate) {
                    Ok(()) => self.autostart = autostart,
                    Err(err) => self.toast(format!("{}: {err}", t.problem_other), ToastKind::Error),
                }
            }
            ui.horizontal(|ui| {
                if self.tray_running {
                    widgets::status_dot(ui, ACCENT);
                    ui.label(RichText::new(t.tray_running).color(TEXT_DIM));
                } else if widgets::secondary_button(ui, t.tray_start_now).clicked() {
                    let args: &[&str] = if self.simulate {
                        &["--tray", "--simulate"]
                    } else {
                        &["--tray"]
                    };
                    if let Err(err) = system::spawn_self(args) {
                        self.toast(format!("{}: {err}", t.problem_other), ToastKind::Error);
                    }
                    self.tray_checked = Some(Instant::now() - Duration::from_millis(1500));
                }
            });
        });

        card(ui, |ui| {
            card_title(ui, t.notif_title, None);
            let mut changed = false;
            let prefs = &mut self.config.notifications;
            changed |= setting_row(ui, t.notif_enabled, None, 60.0, |ui| toggle(ui, &mut prefs.enabled)).changed();
            ui.add_enabled_ui(prefs.enabled, |ui| {
                ui.separator();
                let response = setting_row(ui, t.notif_low, None, 300.0, |ui| {
                    widgets::ltr_box(ui, 300.0, |ui| {
                        ui.add(t.percent_slider(&mut prefs.low_battery_percent, 0..=50))
                    })
                });
                changed |= response.drag_stopped() || (response.changed() && !response.dragged());
                ui.separator();
                changed |= setting_row(ui, t.notif_full, None, 60.0, |ui| toggle(ui, &mut prefs.full_charge)).changed();
                ui.separator();
                changed |= setting_row(ui, t.notif_conn, None, 60.0, |ui| toggle(ui, &mut prefs.connection)).changed();
            });
            if changed {
                self.save_config();
            }
        });

        card(ui, |ui| {
            card_title(ui, t.about_title, None);
            ui.label(
                RichText::new(format!("Synapse for Linux {}", env!("CARGO_PKG_VERSION")))
                    .color(TEXT)
                    .strong(),
            );
            ui.add(Label::new(RichText::new(t.about_text).color(TEXT_DIM)).wrap());
            ui.add(Label::new(RichText::new(t.about_credits).color(TEXT_FAINT).size(12.0)).wrap());
            ui.horizontal(|ui| {
                ui.hyperlink_to(
                    "github.com/bbesli/Synapse-for-Linux",
                    "https://github.com/bbesli/Synapse-for-Linux",
                );
                ui.label(RichText::new("· MIT License").color(TEXT_FAINT).size(12.0));
            });
        });
    }

    fn toasts_ui(&self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-20.0, -20.0))
            .order(Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for toast in &self.toasts {
                    let color = match toast.kind {
                        ToastKind::Success => ACCENT,
                        ToastKind::Error => DANGER,
                        ToastKind::Info => TEXT_DIM,
                    };
                    Frame::new()
                        .fill(CARD)
                        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(14, 10))
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 14,
                            spread: 0,
                            color: Color32::from_black_alpha(140),
                        })
                        .show(ui, |ui| {
                            ui.add(Label::new(RichText::new(&toast.text).color(TEXT)).wrap());
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for SynapseApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync(&ctx);
        let t = self.strings();

        egui::Panel::top("header")
            .exact_size(66.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(20, 0)))
            .show(ui, |ui| self.header(ui, t));

        egui::Panel::left("nav")
            .exact_size(212.0)
            .resizable(false)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin {
                left: 12,
                right: 12,
                top: 16,
                bottom: 12,
            }))
            .show(ui, |ui| self.nav(ui, t));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(24, 18)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 14.0;
                    let device_page = self.page != Page::Settings;
                    if device_page && !self.device_banner(ui, t) {
                        return;
                    }
                    match self.page {
                        Page::Audio => self.page_audio(ui, t),
                        Page::Mic => self.page_mic(ui, t),
                        Page::Power => self.page_power(ui, t),
                        Page::Device => self.page_device(ui, t),
                        Page::Settings => self.page_settings(ui, t),
                    }
                    ui.add_space(8.0);
                });
            });

        self.toasts_ui(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }
}

fn phase_badge(phase: &Phase, t: &Strings) -> (Color32, &'static str) {
    match phase {
        Phase::Ready => (ACCENT, t.status_ready),
        Phase::HeadsetOffline => (WARNING, t.status_offline),
        Phase::Connecting => (TEXT_DIM, t.status_connecting),
        Phase::PermissionDenied(_) => (DANGER, t.status_permission),
        Phase::NoDevice => (DANGER, t.status_no_device),
    }
}

fn banner(ui: &mut Ui, color: Color32, glyph: &str, title: &str, text: Option<&str>) {
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph).size(18.0).color(color));
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).color(TEXT).strong());
                    if let Some(text) = text {
                        ui.add(Label::new(RichText::new(text).color(TEXT_DIM).size(12.5)).wrap());
                    }
                });
            });
        });
}

/// What the headset will report once `request` went through.
fn optimistic(state: &mut HeadsetState, request: &Request) {
    match *request {
        Request::Refresh => {}
        Request::SetSidetone(on) => state.sidetone_enabled = Some(on),
        Request::SetSidetoneLevel(level) => state.sidetone_level = Some(level),
        Request::SetSleepMinutes(minutes) => state.sleep_minutes = Some(minutes),
        Request::SetBtDnd(on) => state.bt_dnd = Some(on),
        Request::SetLedMode(mode) => state.led_mode = Some(mode),
        Request::SelectPreset(preset) => state.eq_preset = Some(preset),
        Request::ApplyCustomEq(bands) => {
            state.eq_preset = Some(HardwarePreset::Custom);
            state.custom_eq = Some(bands);
        }
        Request::SetEnhancement(on) => state.enhancement = Some(on),
        Request::SetEqEnabled(on) => state.eq_enabled = Some(on),
    }
}

/// Install the udev rule through polkit (graphical password prompt).
fn install_udev_rule_with_pkexec() -> Result<(), String> {
    let script = format!(
        "umask 022 && cat > {UDEV_TARGET} && udevadm control --reload-rules && udevadm trigger --subsystem-match=hidraw && udevadm settle"
    );
    let mut child = Command::new("pkexec")
        .args(["/bin/sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("pkexec: {err}"))?;
    child
        .stdin
        .take()
        .ok_or("pkexec: no stdin")?
        .write_all(UDEV_RULE.as_bytes())
        .map_err(|err| format!("pkexec: {err}"))?;
    let output = child.wait_with_output().map_err(|err| format!("pkexec: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("pkexec exited with {}", output.status)
        } else {
            stderr
        })
    }
}
