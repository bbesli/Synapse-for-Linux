# Contributing to Synapse for Linux

> 🇹🇷 **Kısaca:** Katkılarınızı bekliyoruz! Issue ve pull request'leri Türkçe ya da
> İngilizce yazabilirsiniz. En değerli katkı yeni cihaz desteğidir: cihazınızın bilgilerini
> [cihaz talebi](https://github.com/bbesli/Synapse-for-Linux/issues/new?template=device_support.md)
> ile paylaşın. Aşağıdaki "Legal" bölümü önemlidir: Razer'a ait kod, logo ya da dosya
> eklemeyin.

Thank you for helping! This project exists to keep the spirit of Synapse alive on Linux, and
every contribution helps: device support, bug reports, translations, docs or code.

## Ways to help

- **Add a device.** This is the most valuable contribution. Even without writing code, a
  [device support request](https://github.com/bbesli/Synapse-for-Linux/issues/new?template=device_support.md)
  with the information listed below is a big step.
- **Test on your hardware.** The USB-cable mode of the BlackShark V2 HyperSpeed (`1532:056E`)
  has not been tested yet. Any report helps.
- **Report bugs** with the [bug report template](https://github.com/bbesli/Synapse-for-Linux/issues/new?template=bug_report.md).
- **Translate the app.** The UI texts live in [`crates/synapse-gui/src/i18n.rs`](crates/synapse-gui/src/i18n.rs).
- **Features:** a mic equalizer or virtual surround through PipeWire, more Razer headsets,
  packaging for more distributions (Flatpak, .deb, .rpm, AUR).

## Development setup

You need Rust 1.95 or newer ([rustup.rs](https://rustup.rs)). No system libraries are needed
to build.

```bash
git clone https://github.com/bbesli/Synapse-for-Linux.git
cd Synapse-for-Linux
cargo test --workspace                                        # unit tests + simulated end-to-end tests
cargo run -p synapse-gui --bin synapse-linux -- --simulate    # the window, without hardware
cargo run -p synapsectl -- --simulate status                  # the CLI, without hardware
```

`--simulate` (or `SYNAPSE_LINUX_SIMULATE=1`) swaps the real device for an in-memory headset
that answers like the real one, so you can work on the app without the hardware.

The GUI has off-screen render tests. They need a GPU and save PNG screenshots:

```bash
SYNAPSE_SNAPSHOT_DIR=/tmp/shots cargo test -p synapse-gui -- --ignored --test-threads=1
```

Before opening a pull request:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on every pull request.

## Project layout

```
crates/synapse-core/src/
  hid/                  hidraw transport, sysfs discovery, report descriptor parser, cross-process lock
  devices/mod.rs        supported models (MODELS table) and discovery
  devices/blackshark_v2_hs/
    protocol.rs         frame encoding/decoding (pure, fully unit-tested)
    mod.rs              typed driver API (battery(), set_sidetone_level(), apply_custom_eq(), ...)
    sim.rs              simulated device used by tests and --simulate
  manager.rs            background thread shared by the window and the tray
  audio.rs              PipeWire microphone enhancement
  config.rs, eq.rs      settings file and EQ model
crates/synapsectl/      command-line tool
crates/synapse-gui/     window (egui 0.36) and tray (ksni), built as `synapse-linux`
docs/PROTOCOL.md        protocol notes; keep them in sync with code changes
```

## Adding a device

### 1. Collect information (no coding needed)

```bash
lsusb | grep 1532                                         # USB vendor:product ID
for h in /sys/class/hidraw/hidraw*; do echo "$h: $(grep HID_ID $h/device/uevent)"; done
xxd /sys/class/hidraw/hidrawN/device/report_descriptor    # N = your device's hidraw number
```

Put the output in a device support request.

### 2. Find out what Synapse sends

Many Razer devices already have public protocol notes; search first. Otherwise, capture
the USB traffic on Windows while you change one setting at a time in Synapse:

1. Install [Wireshark](https://www.wireshark.org/) with USBPcap.
2. Start a capture on the root hub your device is plugged into.
3. Change a single setting (for example sidetone 5 → 6) and write down the time.
4. Share the `.pcapng` file, or the relevant `SET_REPORT` / interrupt packets, together with
   what you changed.

On Linux, `synapsectl monitor` shows unsolicited reports, and `synapsectl raw <domain> <cmd>
[bytes…]` sends one command to a supported device. Use it to try out read (GET) registers.

### 3. Implement it

- A device that speaks an already supported protocol only needs a new entry in `MODELS`
  (`devices/mod.rs`).
- A new protocol family gets its own module next to `blackshark_v2_hs/` with a pure
  `protocol.rs` (plus unit tests), a driver, and ideally a simulator.
- Record what you verified on real hardware in `docs/PROTOCOL.md`, and list the device in
  both READMEs with its test status.

### Safety rules for hardware work

- **Read before you write.** Try out read (GET) registers first. Never send a write (SET)
  command whose meaning you do not know.
- **Restore what you change.** Settings are stored in the device. Note the original values
  before a test and put them back afterwards.
- **Stay away from firmware and service channels.** Some dongles expose extra vendor
  interfaces, for example the Airoha "RACE" channel on usage page `0xFF13`, that can write
  flash. This project must never use them.

## Code guidelines

- Keep the style consistent: `cargo fmt` (see `rustfmt.toml`) and clippy without warnings.
- Protocol changes need unit tests. The simulator should behave like the real device.
- Never block the UI thread on USB or process I/O. The device manager and background tasks
  exist for that.
- Every user-facing string goes into `i18n.rs` in all languages.
- Avoid new dependencies unless they clearly pay off.
- Keep pull requests small and focused, and describe which hardware you tested on.

## Releasing (maintainers)

1. Update the version in `Cargo.toml` and `packaging/arch/PKGBUILD`, run `cargo build` so
   `Cargo.lock` follows, then add a `CHANGELOG.md` entry and `docs/release-notes/vX.Y.Z.md`.
2. Commit, then tag and push: `git tag -a vX.Y.Z -m "Synapse for Linux X.Y.Z" && git push origin main vX.Y.Z`.
3. Publish the release:
   `gh release create vX.Y.Z --verify-tag --title "Synapse for Linux vX.Y.Z" --notes-file docs/release-notes/vX.Y.Z.md`.
   The *Release* workflow then builds on Ubuntu 22.04 (glibc 2.35), tests and attaches
   `synapse-linux-x86_64.tar.gz` and `SHA256SUMS`. This takes a few minutes; until then the
   quick-install link does not work.

## Legal

- **Do not contribute any Razer property:** no code from Synapse (decompiled or otherwise),
  no Razer logos, product images, sounds, fonts or other files. What devices send over USB,
  and how they respond, may be documented. That knowledge comes from watching the protocol,
  as in [docs/PROTOCOL.md](docs/PROTOCOL.md).
- Refer to Razer products only to say what hardware is supported. Do not suggest that this
  project is official or endorsed by Razer.
- Contributions are licensed under the project's [MIT license](LICENSE).

## Be kind

Be respectful and patient. Everyone here is a volunteer helping Linux users get the most out
of their hardware.
