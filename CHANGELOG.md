# Changelog

All notable changes to this project are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-09-22

First public release.

### Added

- Support for the Razer BlackShark V2 HyperSpeed: 2.4 GHz dongle `1532:0565` (tested on
  hardware), USB cable `1532:056E` (untested), variant `1532:0566` (experimental).
- Equalizer: hardware presets (Music, Game, Movie, Flat, Custom), a drag-and-drop 10-band curve
  editor, built-in curves and saved curves.
- Mic monitoring (sidetone) on/off and levels 0–15.
- Battery level and charging state, a system tray icon, and low-battery and fully-charged
  notifications.
- Auto power-off (1–255 minutes or never), dongle light mode, Bluetooth call blocking, audio
  enhancement and EQ on/off.
- Optional microphone noise suppression through PipeWire and WebRTC.
- `synapsectl` command-line tool with JSON output.
- English and Turkish interface, and a simulation mode for use without hardware.
- udev rule, an in-app "Grant access" flow (polkit), user installers and an Arch Linux PKGBUILD.
