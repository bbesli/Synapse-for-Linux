//! Off-screen renders of every page against the simulated headset.
//!
//! Needs a GPU (wgpu), so it is ignored by default:
//! `SYNAPSE_SNAPSHOT_DIR=/tmp/shots cargo test -p synapse-gui -- --ignored`

use std::time::{Duration, Instant};

use egui_kittest::Harness;

use synapse_core::config::Language;

use crate::app::{Page, SynapseApp};

fn wait_until(harness: &mut Harness<'_, SynapseApp>, what: &str, cond: impl Fn(&SynapseApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.run_steps(2);
        if cond(harness.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

const PAGES: [(Page, &str); 5] = [
    (Page::Audio, "audio"),
    (Page::Mic, "mic"),
    (Page::Power, "power"),
    (Page::Device, "device"),
    (Page::Settings, "settings"),
];

fn harness(simulate: bool) -> Harness<'static, SynapseApp> {
    Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SynapseApp::new(cc, simulate))
}

fn settle(harness: &mut Harness<'_, SynapseApp>) {
    // Let background work (device thread, mic status check) finish.
    for _ in 0..20 {
        harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn save(harness: &mut Harness<'_, SynapseApp>, name: &str) {
    let image = harness.render().expect("render");
    assert_eq!(image.width(), 1100);
    if let Some(dir) = std::env::var_os("SYNAPSE_SNAPSHOT_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        image.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

#[test]
#[ignore = "needs a GPU; run with --ignored"]
fn render_all_pages() {
    let mut harness = harness(true);
    wait_until(&mut harness, "simulated headset", |app| app.is_ready_for_tests());
    for language in [Language::En, Language::Tr] {
        harness.state_mut().set_language_for_tests(language);
        for (page, name) in PAGES {
            harness.state_mut().set_page_for_tests(page);
            settle(&mut harness);
            save(&mut harness, &format!("{name}-{language:?}").to_lowercase());
        }
    }
}

/// Whatever state the real hardware is in (no device / no permission / ready).
/// Read-only: nothing is clicked, so the headset is not changed. The device
/// page is skipped because it shows serial numbers.
#[test]
#[ignore = "needs a GPU; run with --ignored"]
fn render_real_device_state() {
    let mut harness = harness(false);
    settle(&mut harness);
    for language in [Language::Tr, Language::En] {
        harness.state_mut().set_language_for_tests(language);
        for (page, name) in PAGES.into_iter().filter(|(page, _)| *page != Page::Device) {
            harness.state_mut().set_page_for_tests(page);
            settle(&mut harness);
            save(&mut harness, &format!("real-{name}-{language:?}").to_lowercase());
        }
    }
}
