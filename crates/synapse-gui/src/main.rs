//! Synapse for Linux – graphical manager and tray for Razer devices.
//!
//! * `synapse-linux`            open the main window
//! * `synapse-linux --tray`     run the system tray icon (battery, quick settings, notifications)
//! * `synapse-linux --simulate` use a simulated headset (no hardware needed)

mod app;
mod eq_graph;
mod i18n;
mod icon;
mod system;
mod theme;
mod tray;
mod widgets;

#[cfg(test)]
mod render_tests;

use anyhow::anyhow;

const USAGE: &str = "\
Synapse for Linux – Razer device manager

Usage: synapse-linux [OPTIONS]

Options:
  --tray              Run the system tray icon instead of the window
  --simulate          Use a simulated headset (no hardware needed)
  --print-udev-rule   Print the udev rule that grants device access
  -h, --help          Show this help
  -V, --version       Show the version
";

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if let Some(unknown) = args.iter().find(|a| {
        ![
            "--tray",
            "--simulate",
            "--print-udev-rule",
            "-h",
            "--help",
            "-V",
            "--version",
        ]
        .contains(&a.as_str())
    }) {
        eprintln!("unknown option: {unknown}\n\n{USAGE}");
        std::process::exit(2);
    }
    if has("-h") || has("--help") {
        print!("{USAGE}");
        return Ok(());
    }
    if has("-V") || has("--version") {
        println!("synapse-linux {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if has("--print-udev-rule") {
        print!("{}", app::UDEV_RULE);
        return Ok(());
    }

    let simulate = has("--simulate") || synapse_core::devices::simulation_requested();
    if has("--tray") {
        return tray::run(simulate);
    }

    let _instance = match system::single_instance("gui") {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            eprintln!("Synapse for Linux is already open.");
            return Ok(());
        }
        Err(err) => {
            log::warn!("cannot check for another open window ({err}); opening anyway");
            None
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Synapse for Linux")
            .with_app_id("synapse-linux")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "synapse-linux",
        options,
        Box::new(move |cc| Ok(Box::new(app::SynapseApp::new(cc, simulate)))),
    )
    .map_err(|err| anyhow!("cannot open the window: {err}"))
}
