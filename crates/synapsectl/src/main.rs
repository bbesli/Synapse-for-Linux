//! `synapsectl` – command line control for Razer devices.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use synapse_core::Error;
use synapse_core::audio;
use synapse_core::config::Config;
use synapse_core::devices::blackshark_v2_hs::protocol;
use synapse_core::devices::blackshark_v2_hs::{HeadsetState, Incoming, LedMode};
use synapse_core::devices::{self, BlackSharkV2Hs, Connection, Device, FoundDevice, Location};
use synapse_core::eq::{self, Bands, HardwarePreset};
use synapse_core::hid::Transport;
use synapse_core::manager::format_clock;

const UDEV_RULE: &str = include_str!("../../../packaging/udev/70-synapse-linux.rules");

#[derive(Parser)]
#[command(
    name = "synapsectl",
    version,
    about = "Control Razer devices on Linux (Synapse for Linux)"
)]
struct Cli {
    /// Print machine readable JSON
    #[arg(long, global = true)]
    json: bool,

    /// Use a simulated headset instead of real hardware
    #[arg(long, global = true)]
    simulate: bool,

    /// Device to use when several are connected: index from `list` or /dev/hidrawN
    #[arg(long, short, global = true)]
    device: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List supported devices that are plugged in
    List,
    /// Show status and every setting (default)
    Status,
    /// Battery level and charging state
    Battery,
    /// Equalizer: show it, select a preset or write a custom curve
    Eq {
        #[command(subcommand)]
        action: Option<EqAction>,
    },
    /// Microphone monitoring (sidetone): on, off or a level 0-15
    Sidetone { value: String },
    /// Automatic power-off: minutes (1-255) or "off"
    Sleep { value: String },
    /// Dongle LED: link, battery, warning or off
    Led { mode: String },
    /// Block Bluetooth calls while the 2.4 GHz link is active: on/off
    Dnd { value: String },
    /// Print events from the device as they arrive (Ctrl+C to stop)
    Monitor {
        /// Also read link/battery every N seconds
        #[arg(long, default_value_t = 0)]
        poll: u64,
    },
    /// Send a raw command: DOMAIN CMD [BYTES...] (debugging; values in hex like 0x80 or decimal)
    Raw {
        domain: String,
        cmd: String,
        #[arg(num_args = 0..)]
        bytes: Vec<String>,
    },
    /// Software microphone noise suppression through PipeWire: on, off, default or status
    MicClean {
        #[arg(default_value = "status")]
        action: String,
    },
    /// Print the udev rule that grants access without root
    UdevRule,
}

#[derive(Subcommand)]
enum EqAction {
    /// Select a hardware preset: music, game, movie, flat or custom
    Preset { name: String },
    /// Write 10 gains in dB (-9..6) to the custom slot, e.g. `eq set 3 2 1 0 0 0 1 2 3 2`
    Set {
        #[arg(num_args = 1.., allow_negative_numbers = true, value_name = "DB")]
        bands: Vec<String>,
    },
    /// Apply a built-in curve (bass, footsteps, voice, treble) or a curve saved in the app
    Curve { name: String },
    /// List presets and curves
    List,
    /// Turn the EQ stage on or off
    Enable { value: String },
    /// DSP "audio enhancement" (bass/spatial expander) on or off
    Enhancement { value: String },
}

type Headset = BlackSharkV2Hs<Box<dyn Transport>>;

fn main() -> ExitCode {
    // Behave like other command line tools when piped into `head` etc.
    // SAFETY: restoring the default signal disposition at startup.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match cli.command.as_ref().unwrap_or(&Command::Status) {
        Command::List => list(cli),
        Command::UdevRule => {
            print!("{UDEV_RULE}");
            Ok(())
        }
        Command::Status => {
            let (found, mut dev) = open(cli)?;
            let state = dev.read_state()?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({ "device": found.model, "path": found.path_display(), "state": state })
                    )?
                );
            } else {
                print_status(&found, &state);
            }
            Ok(())
        }
        Command::Battery => {
            let (_, mut dev) = open(cli)?;
            ensure_online(&mut dev)?;
            let level = dev.battery()?;
            let charging = dev.charging()?;
            if cli.json {
                println!("{}", json!({ "battery": level, "charging": charging }));
            } else {
                println!("{level}%{}", if charging { " (charging)" } else { "" });
            }
            Ok(())
        }
        Command::Eq { action } => eq_command(cli, action.as_ref()),
        Command::Sidetone { value } => {
            let (_, mut dev) = open(cli)?;
            ensure_online(&mut dev)?;
            // A number is a level (0 included); words switch it on or off.
            match value.trim().parse::<u8>() {
                Ok(level) if level <= protocol::SIDETONE_MAX => {
                    dev.set_sidetone_level(level)?;
                    if level > 0 {
                        dev.set_sidetone_enabled(true)?;
                    }
                }
                Ok(_) => bail!("the sidetone level must be 0-{}", protocol::SIDETONE_MAX),
                Err(_) => match parse_switch(value) {
                    Some(on) => dev.set_sidetone_enabled(on)?,
                    None => bail!("expected on, off or a level 0-{}", protocol::SIDETONE_MAX),
                },
            }
            let (on, level) = (dev.sidetone_enabled()?, dev.sidetone_level()?);
            report(
                cli,
                json!({ "sidetone": on, "level": level }),
                format!("sidetone {} (level {level}/{})", on_off(on), protocol::SIDETONE_MAX),
            )
        }
        Command::Sleep { value } => {
            let minutes = match value.as_str() {
                v if parse_switch(v) == Some(false) || v == "never" => 0,
                v => v.parse::<u8>().map_err(|_| anyhow!("expected minutes 1-255 or off"))?,
            };
            let (_, mut dev) = open(cli)?;
            ensure_online(&mut dev)?;
            dev.set_sleep_minutes(minutes)?;
            let now = dev.sleep_minutes()?;
            report(
                cli,
                json!({ "sleep_minutes": now }),
                format!("auto power-off: {}", minutes_text(now)),
            )
        }
        Command::Led { mode } => {
            let mode = LedMode::from_key(mode).ok_or_else(|| anyhow!("expected link, battery, warning or off"))?;
            let (_, mut dev) = open(cli)?;
            dev.set_led_mode(mode)?;
            let now = dev.led_mode()?.map(LedMode::key).unwrap_or_default();
            report(cli, json!({ "led": now }), format!("dongle LED: {now}"))
        }
        Command::Dnd { value } => {
            let on = parse_switch(value).ok_or_else(|| anyhow!("expected on or off"))?;
            let (_, mut dev) = open(cli)?;
            ensure_online(&mut dev)?;
            dev.set_bt_dnd(on)?;
            let now = dev.bt_dnd()?;
            report(
                cli,
                json!({ "bt_dnd": now }),
                format!("block Bluetooth calls: {}", on_off(now)),
            )
        }
        Command::MicClean { action } => mic_clean(cli, action),
        Command::Monitor { poll } => monitor(cli, *poll),
        Command::Raw { domain, cmd, bytes } => raw(cli, domain, cmd, bytes),
    }
}

fn report(cli: &Cli, value: serde_json::Value, text: String) -> Result<()> {
    if cli.json {
        println!("{value}");
    } else {
        println!("{text}");
    }
    Ok(())
}

// ----- device selection ---------------------------------------------------

fn candidates(cli: &Cli) -> Vec<FoundDevice> {
    if cli.simulate {
        devices::simulated()
    } else {
        devices::discover()
    }
}

fn open(cli: &Cli) -> Result<(FoundDevice, Headset)> {
    let found = candidates(cli);
    if found.is_empty() {
        bail!("no supported Razer device found (is the dongle plugged in?)");
    }
    let chosen = match &cli.device {
        None => found
            .iter()
            .find(|f| !f.model.experimental)
            .unwrap_or(&found[0])
            .clone(),
        Some(sel) => match sel.parse::<usize>() {
            Ok(index) => found
                .get(index)
                .cloned()
                .ok_or_else(|| anyhow!("no device #{index}, see `synapsectl list`"))?,
            Err(_) => found
                .iter()
                .find(|f| f.path_display() == *sel || f.path_display().ends_with(&format!("/{sel}")))
                .cloned()
                .ok_or_else(|| anyhow!("{sel} is not a supported device, see `synapsectl list`"))?,
        },
    };
    match devices::open(&chosen) {
        Ok(Device::BlackSharkV2Hs(dev)) => Ok((chosen, dev)),
        Err(Error::PermissionDenied { path }) => Err(anyhow!(permission_help(&path))),
        Err(err) => Err(err).with_context(|| format!("cannot open {}", chosen.path_display())),
    }
}

fn permission_help(path: &Path) -> String {
    format!(
        "no permission to open {}.\n\
         Install the udev rule once (needs sudo), then re-plug the dongle or run the trigger:\n\n  \
         synapsectl udev-rule | sudo tee /etc/udev/rules.d/70-synapse-linux.rules >/dev/null\n  \
         sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=hidraw",
        path.display()
    )
}

fn ensure_online(dev: &mut Headset) -> Result<()> {
    if dev.link_up()? {
        Ok(())
    } else {
        Err(Error::HeadsetOffline.into())
    }
}

fn list(cli: &Cli) -> Result<()> {
    let found = candidates(cli);
    if cli.json {
        let items: Vec<_> = found
            .iter()
            .map(|f| json!({ "model": f.model, "path": f.path_display(), "accessible": accessible(f) }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if found.is_empty() {
        println!("No supported Razer device found.");
        return Ok(());
    }
    for (i, f) in found.iter().enumerate() {
        println!(
            "#{i}  {} [{:04x}:{:04x}] {}  {}{}{}",
            f.model.name,
            f.model.vid,
            f.model.pid,
            connection_text(f.model.connection),
            f.path_display(),
            if accessible(f) {
                ""
            } else {
                "  (no permission, run `synapsectl udev-rule`)"
            },
            if f.model.experimental { "  (experimental)" } else { "" },
        );
    }
    Ok(())
}

fn accessible(found: &FoundDevice) -> bool {
    match &found.location {
        Location::Simulated => true,
        Location::Hidraw(node) => {
            let Ok(path) = std::ffi::CString::new(node.devnode.as_os_str().as_encoded_bytes()) else {
                return false;
            };
            // SAFETY: valid NUL-terminated path.
            unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
        }
    }
}

// ----- equalizer ------------------------------------------------------------

fn eq_command(cli: &Cli, action: Option<&EqAction>) -> Result<()> {
    if let Some(EqAction::List) = action {
        return eq_list(cli);
    }
    let (_, mut dev) = open(cli)?;
    ensure_online(&mut dev)?;
    match action {
        None | Some(EqAction::List) => {}
        Some(EqAction::Preset { name }) => {
            let preset = HardwarePreset::from_key(name)
                .ok_or_else(|| anyhow!("unknown preset {name:?}; use music, game, movie, flat or custom"))?;
            dev.select_preset(preset)?;
        }
        Some(EqAction::Set { bands }) => {
            let bands = parse_bands(bands)?;
            dev.apply_custom_eq(&bands)?;
            if !cli.simulate {
                remember_curve(bands);
            }
        }
        Some(EqAction::Curve { name }) => {
            let bands = named_curve(name)?;
            dev.apply_custom_eq(&bands)?;
            if !cli.simulate {
                remember_curve(bands);
            }
        }
        Some(EqAction::Enable { value }) => {
            dev.set_eq_enabled(parse_switch(value).ok_or_else(|| anyhow!("expected on or off"))?)?
        }
        Some(EqAction::Enhancement { value }) => {
            dev.set_enhancement(parse_switch(value).ok_or_else(|| anyhow!("expected on or off"))?)?
        }
    }
    let preset = dev.eq_preset()?;
    let bands = dev.custom_eq()?;
    let enabled = dev.eq_enabled()?;
    let enhancement = dev.enhancement()?;
    if cli.json {
        println!(
            "{}",
            json!({ "preset": preset.key(), "custom_eq": bands, "enabled": enabled, "enhancement": enhancement })
        );
    } else {
        println!("preset       {}", preset_text(preset));
        if let Some(curve) = preset.reference_curve().filter(|_| preset != HardwarePreset::Custom) {
            println!("factory      {}  (approximation)", bands_text(&curve));
        }
        println!("custom slot  {}", bands_text(&bands));
        println!("EQ stage     {}", on_off(enabled));
        println!("enhancement  {}", on_off(enhancement));
    }
    Ok(())
}

fn eq_list(cli: &Cli) -> Result<()> {
    let config = Config::load();
    if cli.json {
        let rom: Vec<_> = HardwarePreset::SELECTABLE
            .iter()
            .map(|p| json!({ "key": p.key(), "curve": p.reference_curve() }))
            .collect();
        let builtin: Vec<_> = eq::BUILTIN_CURVES
            .iter()
            .map(|c| json!({ "key": c.key, "name": c.name_en, "bands": c.bands }))
            .collect();
        let saved: Vec<_> = config
            .user_presets
            .iter()
            .map(|p| json!({ "name": p.name, "bands": p.bands }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "presets": rom, "curves": builtin, "saved": saved }))?
        );
        return Ok(());
    }
    println!("Hardware presets (eq preset NAME):");
    for p in HardwarePreset::SELECTABLE {
        let curve = p
            .reference_curve()
            .map(|c| bands_text(&c))
            .unwrap_or_else(|| "(your curve)".into());
        println!("  {:<8} {curve}", p.key());
    }
    println!("\nBuilt-in curves (eq curve NAME):");
    for c in eq::BUILTIN_CURVES {
        println!("  {:<10} {}  {}", c.key, bands_text(&c.bands), c.name_en);
    }
    if !config.user_presets.is_empty() {
        println!("\nSaved curves (eq curve \"NAME\"):");
        for p in &config.user_presets {
            println!("  {:<16} {}", p.name, bands_text(&p.bands));
        }
    }
    println!(
        "\nBands: {}",
        (0..eq::BAND_COUNT)
            .map(|i| format!("{}Hz", eq::band_label(i)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(())
}

fn named_curve(name: &str) -> Result<Bands> {
    if let Some(curve) = eq::builtin_curve(name) {
        return Ok(curve.bands);
    }
    Config::load()
        .find_preset(name)
        .map(|p| p.bands)
        .ok_or_else(|| anyhow!("no curve named {name:?}; see `synapsectl eq list`"))
}

fn parse_bands(args: &[String]) -> Result<Bands> {
    let values: Vec<i32> = args
        .iter()
        .flat_map(|a| a.split(',').map(str::trim).filter(|s| !s.is_empty()))
        .map(|s| {
            s.trim_start_matches('+')
                .parse::<i32>()
                .map_err(|_| anyhow!("{s:?} is not a number"))
        })
        .collect::<Result<_>>()?;
    if values.len() != eq::BAND_COUNT {
        bail!(
            "expected {} values ({}), got {}",
            eq::BAND_COUNT,
            eq_band_names(),
            values.len()
        );
    }
    if let Some(v) = values
        .iter()
        .find(|v| !(i32::from(eq::MIN_GAIN_DB)..=i32::from(eq::MAX_GAIN_DB)).contains(v))
    {
        bail!(
            "{v} dB is outside the hardware range {}..{} dB",
            eq::MIN_GAIN_DB,
            eq::MAX_GAIN_DB
        );
    }
    let mut bands = [0i8; eq::BAND_COUNT];
    for (band, value) in bands.iter_mut().zip(values) {
        *band = value as i8;
    }
    Ok(bands)
}

fn eq_band_names() -> String {
    (0..eq::BAND_COUNT).map(eq::band_label).collect::<Vec<_>>().join(" ")
}

fn remember_curve(bands: Bands) {
    let mut config = Config::load();
    config.last_custom_curve = Some(bands);
    if let Err(err) = config.save() {
        log::warn!("cannot save config: {err}");
    }
}

// ----- PipeWire microphone enhancement ---------------------------------------

fn mic_clean(cli: &Cli, action: &str) -> Result<()> {
    match action {
        "status" => {}
        "default" => audio::set_default_source(audio::CLEAN_NODE).map_err(|e| anyhow!(e))?,
        other => match parse_switch(other) {
            Some(true) => {
                let source = audio::find_razer_source()
                    .ok_or_else(|| anyhow!("no Razer microphone found in PipeWire (is the headset connected?)"))?;
                audio::enable(&source.name, "Razer Mic (Clean)").map_err(|e| anyhow!(e))?;
                // PipeWire needs a moment to publish the new source.
                std::thread::sleep(Duration::from_millis(600));
            }
            Some(false) => audio::disable().map_err(|e| anyhow!(e))?,
            None => bail!("expected on, off, default or status"),
        },
    }
    let status = audio::status();
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    if !status.plugin_available {
        println!("PipeWire WebRTC plugin: not installed (noise suppression unavailable)");
    }
    println!("clean microphone  {}", if status.active { "running" } else { "off" });
    println!("start with login  {}", on_off(status.enabled));
    if let Some(target) = &status.target {
        println!("source            {target}");
    }
    if status.active {
        println!(
            "default input     {}",
            if status.is_default {
                "yes"
            } else {
                "no (run: synapsectl mic-clean default)"
            }
        );
    }
    Ok(())
}

// ----- monitor / raw ----------------------------------------------------------

fn monitor(cli: &Cli, poll: u64) -> Result<()> {
    let (found, mut dev) = open(cli)?;
    eprintln!("Listening on {} (Ctrl+C to stop)...", found.path_display());
    let poll = (poll > 0).then(|| Duration::from_secs(poll));
    let mut next_poll = Instant::now();
    loop {
        if let Some(interval) = poll {
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + interval;
                let link = dev.link_up()?;
                let battery = if link { dev.battery().ok() } else { None };
                println!(
                    "{}  poll: link {}, battery {}",
                    format_clock(std::time::SystemTime::now()),
                    if link { "up" } else { "down" },
                    battery.map(|b| format!("{b}%")).unwrap_or_else(|| "-".into())
                );
            }
        }
        for event in dev.poll_incoming(Duration::from_millis(250))? {
            let time = format_clock(std::time::SystemTime::now());
            match event {
                Incoming::Frame(r) => println!(
                    "{time}  frame: domain 0x{:02X} cmd 0x{:02X} status 0x{:02X} seq 0x{:02X} payload [{}]",
                    r.domain,
                    r.cmd,
                    r.status,
                    r.seq,
                    hex(&r.payload)
                ),
                Incoming::Other { report_id, data } => {
                    let trimmed = data
                        .iter()
                        .rposition(|b| *b != 0)
                        .map_or(&data[..0], |end| &data[..=end]);
                    println!("{time}  report 0x{report_id:02X}: [{}]", hex(trimmed));
                }
            }
        }
    }
}

fn raw(cli: &Cli, domain: &str, cmd: &str, bytes: &[String]) -> Result<()> {
    let domain = parse_byte(domain)?;
    let cmd = parse_byte(cmd)?;
    let params: Vec<u8> = bytes.iter().map(|b| parse_byte(b)).collect::<Result<_>>()?;
    let (_, mut dev) = open(cli)?;
    let resp = dev.transact(domain, cmd, &params)?;
    if cli.json {
        println!(
            "{}",
            json!({ "seq": resp.seq, "domain": resp.domain, "cmd": resp.cmd, "status": resp.status, "payload": resp.payload })
        );
    } else {
        println!(
            "status 0x{:02X}, {} byte(s): [{}]  {:?}",
            resp.status,
            resp.payload.len(),
            hex(&resp.payload),
            String::from_utf8_lossy(&resp.payload)
        );
    }
    Ok(())
}

fn parse_byte(text: &str) -> Result<u8> {
    let t = text.trim();
    let value = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => t.parse::<u8>(),
    };
    value.map_err(|_| anyhow!("{text:?} is not a byte (use 0x.. hex or 0-255)"))
}

// ----- output helpers ---------------------------------------------------------

fn print_status(found: &FoundDevice, s: &HeadsetState) {
    println!(
        "{}  ({}, {})",
        found.model.name,
        connection_text(found.model.connection),
        found.path_display()
    );
    if s.link_up != Some(true) {
        println!("  Headset         not connected (powered off or out of range)");
    } else {
        let battery = s.battery.map(|b| format!("{b}%")).unwrap_or_else(|| "?".into());
        let charging = match s.charging {
            Some(true) => " (charging)",
            Some(false) => " (on battery)",
            None => "",
        };
        println!("  Battery         {battery}{charging}");
        println!(
            "  Microphone      {}",
            opt(s.mic_muted.map(|m| if m { "muted" } else { "on" }))
        );
        println!(
            "  Sidetone        {} (level {}/{})",
            opt(s.sidetone_enabled.map(on_off)),
            opt(s.sidetone_level),
            protocol::SIDETONE_MAX
        );
        println!("  EQ preset       {}", opt(s.eq_preset.map(preset_text)));
        println!("  Custom EQ slot  {}", opt(s.custom_eq.as_ref().map(bands_text)));
        println!(
            "  EQ stage        {}, enhancement {}",
            opt(s.eq_enabled.map(on_off)),
            opt(s.enhancement.map(on_off))
        );
        println!("  Auto power-off  {}", opt(s.sleep_minutes.map(minutes_text)));
        println!("  Block BT calls  {}", opt(s.bt_dnd.map(on_off)));
    }
    if found.model.connection == Connection::Dongle {
        println!("  Dongle LED      {}", opt(s.led_mode.map(LedMode::key)));
    }
    let fw = [("headset", &s.headset_firmware), ("dongle", &s.dongle_firmware)];
    let serial = [("headset", &s.headset_serial), ("dongle", &s.dongle_serial)];
    let join = |items: &[(&str, &Option<String>)]| {
        items
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  Firmware        {}", join(&fw));
    println!("  Serial          {}", join(&serial));
}

fn opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn parse_switch(text: &str) -> Option<bool> {
    match text.trim().to_lowercase().as_str() {
        "on" | "1" | "true" | "yes" | "enable" | "acik" | "açık" | "ac" | "aç" => Some(true),
        "off" | "0" | "false" | "no" | "disable" | "kapali" | "kapalı" | "kapat" => Some(false),
        _ => None,
    }
}

fn minutes_text(minutes: u8) -> String {
    if minutes == 0 {
        "never".into()
    } else {
        format!("{minutes} min")
    }
}

fn connection_text(connection: Connection) -> &'static str {
    match connection {
        Connection::Dongle => "2.4 GHz dongle",
        Connection::Wired => "USB cable",
    }
}

fn preset_text(preset: HardwarePreset) -> String {
    match preset {
        HardwarePreset::Custom => "custom".into(),
        HardwarePreset::Unknown(raw) => format!("unknown (0x{raw:02X})"),
        rom => format!("{} (factory)", rom.key()),
    }
}

fn bands_text(bands: &Bands) -> String {
    let values: Vec<String> = bands.iter().map(|b| format!("{b:+}")).collect();
    format!("[{}] dB", values.join(" "))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_parsing() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            parse_bands(&args("3 2 1 0 0 0 1 2 3 2")).unwrap(),
            [3, 2, 1, 0, 0, 0, 1, 2, 3, 2]
        );
        assert_eq!(
            parse_bands(&args("-9,+6,0,0,0 0,0,0,0,-1")).unwrap(),
            [-9, 6, 0, 0, 0, 0, 0, 0, 0, -1]
        );
        assert!(parse_bands(&args("1 2 3")).is_err());
        assert!(parse_bands(&args("7 0 0 0 0 0 0 0 0 0")).is_err());
        assert!(parse_bands(&args("x 0 0 0 0 0 0 0 0 0")).is_err());
    }

    #[test]
    fn switches_and_bytes() {
        assert_eq!(parse_switch("ON"), Some(true));
        assert_eq!(parse_switch("kapalı"), Some(false));
        assert_eq!(parse_switch("7"), None);
        assert_eq!(parse_byte("0x80").unwrap(), 0x80);
        assert_eq!(parse_byte("33").unwrap(), 33);
        assert!(parse_byte("0x100").is_err());
    }

    #[test]
    fn cli_parses_negative_eq_values() {
        let cli = Cli::try_parse_from([
            "synapsectl",
            "eq",
            "set",
            "-9",
            "-3",
            "0",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "0",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Eq {
                action: Some(EqAction::Set { bands }),
            }) => assert_eq!(bands.len(), 10),
            _ => panic!("wrong parse"),
        }
    }
}
