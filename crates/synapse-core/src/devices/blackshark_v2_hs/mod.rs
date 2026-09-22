//! Driver for the MediaTek based Razer BlackShark V2 HyperSpeed.
//!
//! Everything Synapse configures on this headset lives in the headset itself
//! (EQ, sidetone, sleep timer, ...), so settings made here persist and also
//! apply on consoles and phones.

pub mod protocol;
pub mod sim;

use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use self::protocol::{DOMAIN_HEADSET_RF, DOMAIN_LOCAL, DecodeError, Response, reg};
use super::{Connection, Model};
use crate::eq::{Bands, HardwarePreset};
use crate::hid::Transport;
use crate::hid::lock::DeviceLock;
use crate::{Error, Result};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(800);
const LOCK_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_QUEUED_EVENTS: usize = 64;

/// Dongle status LED behaviour (register 0x66 / 0xE6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedMode {
    Off,
    /// White while the headset is linked.
    Link,
    /// Green / yellow / red by headset battery level.
    Battery,
    /// Only blink when the battery is low.
    Warning,
    Unknown(u8),
}

impl LedMode {
    pub const ALL: [LedMode; 4] = [LedMode::Link, LedMode::Battery, LedMode::Warning, LedMode::Off];

    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Off,
            1 => Self::Link,
            2 => Self::Battery,
            3 => Self::Warning,
            other => Self::Unknown(other),
        }
    }

    pub fn raw(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Link => 1,
            Self::Battery => 2,
            Self::Warning => 3,
            Self::Unknown(raw) => raw,
        }
    }

    pub fn key(self) -> String {
        match self {
            Self::Off => "off".into(),
            Self::Link => "link".into(),
            Self::Battery => "battery".into(),
            Self::Warning => "warning".into(),
            Self::Unknown(raw) => format!("{raw}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "off" | "0" => Some(Self::Off),
            "link" | "1" => Some(Self::Link),
            "battery" | "2" => Some(Self::Battery),
            "warning" | "3" => Some(Self::Warning),
            _ => None,
        }
    }
}

/// Everything known about the headset. `None` = not read (yet).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HeadsetState {
    /// Headset reachable (always true for a wired connection).
    pub link_up: Option<bool>,
    pub battery: Option<u8>,
    pub charging: Option<bool>,
    pub headset_serial: Option<String>,
    pub headset_firmware: Option<String>,
    pub dongle_serial: Option<String>,
    pub dongle_firmware: Option<String>,
    pub sidetone_enabled: Option<bool>,
    pub sidetone_level: Option<u8>,
    pub sleep_minutes: Option<u8>,
    pub bt_dnd: Option<bool>,
    pub mic_muted: Option<bool>,
    pub led_mode: Option<LedMode>,
    pub eq_preset: Option<HardwarePreset>,
    /// Curve stored in the Custom slot. Register 0x15 always returns this,
    /// whichever preset is active (verified on hardware).
    pub custom_eq: Option<Bands>,
    pub eq_enabled: Option<bool>,
    pub enhancement: Option<bool>,
}

impl HeadsetState {
    /// Forget everything that belongs to the headset (keeps dongle info).
    pub fn clear_headset(&mut self) {
        *self = HeadsetState {
            link_up: self.link_up,
            dongle_serial: self.dongle_serial.take(),
            dongle_firmware: self.dongle_firmware.take(),
            led_mode: self.led_mode,
            ..HeadsetState::default()
        };
    }

    /// Apply a GET reply (solicited or not) to the state.
    /// Returns false when the frame carries nothing we track.
    pub fn apply_reply(&mut self, resp: &Response, connection: Connection) -> bool {
        if reg::is_set(resp.cmd) {
            return false;
        }
        let local = resp.domain == DOMAIN_LOCAL && connection == Connection::Dongle;
        let first = resp.payload.first().copied();
        match (resp.cmd, first) {
            (reg::LINK_STATUS, Some(v)) if local || connection == Connection::Wired => {
                self.link_up = Some(v != 0 || connection == Connection::Wired)
            }
            (reg::DONGLE_LED, Some(v)) => self.led_mode = Some(LedMode::from_raw(v)),
            (reg::SERIAL, _) if local => self.dongle_serial = protocol::format_serial(&resp.payload),
            (reg::FIRMWARE, _) if local => self.dongle_firmware = protocol::format_firmware(&resp.payload),
            // Everything below belongs to the headset; ignore dongle-local copies.
            _ if local => return false,
            (reg::SERIAL, _) => self.headset_serial = protocol::format_serial(&resp.payload),
            (reg::FIRMWARE, _) => self.headset_firmware = protocol::format_firmware(&resp.payload),
            (reg::BATTERY, Some(v)) if v <= 100 => self.battery = Some(v),
            (reg::CHARGING, Some(v)) => self.charging = Some(v != 0),
            (reg::SIDETONE, Some(v)) => self.sidetone_enabled = Some(v != 0),
            (reg::SIDETONE_LEVEL, Some(v)) => self.sidetone_level = Some(v.min(protocol::SIDETONE_MAX)),
            (reg::SLEEP_TIMER, Some(v)) => self.sleep_minutes = Some(v),
            (reg::BT_DND, Some(v)) => self.bt_dnd = Some(v != 0),
            (reg::MIC_MUTE, Some(v)) => self.mic_muted = Some(v != 0),
            (reg::EQ_PRESET, Some(v)) => self.eq_preset = Some(HardwarePreset::from_raw(v)),
            (reg::EQ_BANDS, _) => match protocol::eq_from_payload(&resp.payload) {
                Some(bands) => self.custom_eq = Some(bands),
                None => return false,
            },
            (reg::EQ_ENABLE, Some(v)) => self.eq_enabled = Some(v != 0),
            (reg::ENHANCEMENT, Some(v)) => self.enhancement = Some(v != 0),
            _ => return false,
        }
        true
    }
}

/// Something the device sent without us asking (or a reply meant for
/// another process sharing the device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Frame(Response),
    /// Any other report (media keys 0x0C, telephony 0x05, ...).
    Other {
        report_id: u8,
        data: Vec<u8>,
    },
}

pub struct BlackSharkV2Hs<T: Transport> {
    transport: T,
    model: &'static Model,
    counter: u8,
    timeout: Duration,
    lock: Option<DeviceLock>,
    incoming: VecDeque<Incoming>,
}

impl<T: Transport> BlackSharkV2Hs<T> {
    pub fn new(transport: T, model: &'static Model) -> Self {
        let lock = transport.lock_key().and_then(|key| match DeviceLock::for_key(&key) {
            Ok(lock) => Some(lock),
            Err(err) => {
                log::warn!("cannot create device lock file: {err}");
                None
            }
        });
        // Start at a per-process offset so concurrent tools rarely share sequence numbers.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let counter = (std::process::id() as u8) ^ (nanos >> 10) as u8;
        let timeout = std::env::var("SYNAPSE_LINUX_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        Self {
            transport,
            model,
            counter,
            timeout,
            lock,
            incoming: VecDeque::new(),
        }
    }

    pub fn model(&self) -> &'static Model {
        self.model
    }

    pub fn connection(&self) -> Connection {
        self.model.connection
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Domain that reaches the headset: relayed over RF on the dongle,
    /// local when the headset itself is plugged in.
    pub fn headset_domain(&self) -> u8 {
        match self.model.connection {
            Connection::Dongle => DOMAIN_HEADSET_RF,
            Connection::Wired => DOMAIN_LOCAL,
        }
    }

    /// Send one command and wait for its reply.
    pub fn transact(&mut self, domain: u8, cmd: u8, params: &[u8]) -> Result<Response> {
        let _guard = match &self.lock {
            Some(lock) => Some(lock.acquire(LOCK_TIMEOUT)?),
            None => None,
        };

        // Keep anything that arrived meanwhile, so stale replies can't be
        // mistaken for ours and unsolicited events are not lost.
        self.pump(Duration::ZERO)?;

        self.counter = self.counter.wrapping_add(1);
        let seq = protocol::sequence_byte(self.counter);
        let frame = protocol::encode_request(seq, domain, cmd, params).map_err(Error::InvalidArgument)?;
        log::trace!("-> {}", hex(&frame[..13 + params.len()]));
        self.transport.write_report(&frame)?;

        let deadline = Instant::now() + self.timeout;
        let mut buf = [0u8; 128];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout {
                    cmd,
                    timeout: self.timeout,
                });
            }
            let Some(n) = self.transport.read_report(&mut buf, remaining)? else {
                continue;
            };
            match protocol::decode_response(&buf[..n]) {
                Ok(resp) if resp.answers(seq, cmd) => {
                    log::trace!("<- {}", hex(&buf[..n.min(13 + resp.payload.len())]));
                    if resp.status != 0x01 {
                        log::debug!("reply to 0x{cmd:02X} has status 0x{:02X}", resp.status);
                    }
                    return Ok(resp);
                }
                Ok(resp) => self.queue(Incoming::Frame(resp)),
                Err(DecodeError::OtherReport(id)) => self.queue(Incoming::Other {
                    report_id: id,
                    data: buf[1..n].to_vec(),
                }),
                Err(err) => log::debug!("ignoring malformed frame: {err}: {}", hex(&buf[..n])),
            }
        }
    }

    /// Read whatever the device sent on its own (non-blocking beyond `wait`).
    pub fn poll_incoming(&mut self, wait: Duration) -> Result<Vec<Incoming>> {
        self.pump(wait)?;
        Ok(self.incoming.drain(..).collect())
    }

    fn pump(&mut self, wait: Duration) -> Result<()> {
        let mut buf = [0u8; 128];
        let mut timeout = wait;
        while let Some(n) = self.transport.read_report(&mut buf, timeout)? {
            timeout = Duration::ZERO;
            match protocol::decode_response(&buf[..n]) {
                Ok(resp) => self.queue(Incoming::Frame(resp)),
                Err(DecodeError::OtherReport(id)) => self.queue(Incoming::Other {
                    report_id: id,
                    data: buf[1..n].to_vec(),
                }),
                Err(err) => log::debug!("ignoring malformed frame: {err}"),
            }
        }
        Ok(())
    }

    fn queue(&mut self, event: Incoming) {
        if self.incoming.len() >= MAX_QUEUED_EVENTS {
            self.incoming.pop_front();
        }
        self.incoming.push_back(event);
    }

    fn get(&mut self, domain: u8, cmd: u8) -> Result<Response> {
        let resp = self.transact(domain, cmd, &[])?;
        if resp.payload.is_empty() {
            return Err(Error::EmptyResponse { cmd });
        }
        Ok(resp)
    }

    fn get_u8(&mut self, domain: u8, cmd: u8) -> Result<u8> {
        Ok(self.get(domain, cmd)?.payload[0])
    }

    fn set_u8(&mut self, domain: u8, get_cmd: u8, value: u8) -> Result<()> {
        self.transact(domain, reg::set(get_cmd), &[value]).map(drop)
    }

    fn headset_get_u8(&mut self, cmd: u8) -> Result<u8> {
        let domain = self.headset_domain();
        self.get_u8(domain, cmd)
    }

    fn headset_set_u8(&mut self, cmd: u8, value: u8) -> Result<()> {
        let domain = self.headset_domain();
        self.set_u8(domain, cmd, value)
    }

    // ----- identity -------------------------------------------------------

    /// Is the headset reachable? Always true for a wired connection.
    pub fn link_up(&mut self) -> Result<bool> {
        if self.model.connection == Connection::Wired {
            return Ok(true);
        }
        match self.get_u8(DOMAIN_LOCAL, reg::LINK_STATUS) {
            Ok(v) => Ok(v != 0),
            // A dongle that does not answer this locally: ask the headset instead.
            Err(err) if err.is_timeout() => match self.battery() {
                Ok(_) => Ok(true),
                Err(err) if err.is_timeout() => Ok(false),
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        }
    }

    pub fn headset_serial(&mut self) -> Result<Option<String>> {
        let domain = self.headset_domain();
        Ok(protocol::format_serial(&self.get(domain, reg::SERIAL)?.payload))
    }

    pub fn headset_firmware(&mut self) -> Result<Option<String>> {
        let domain = self.headset_domain();
        Ok(protocol::format_firmware(&self.get(domain, reg::FIRMWARE)?.payload))
    }

    /// Serial of the dongle itself (`None` when wired).
    pub fn dongle_serial(&mut self) -> Result<Option<String>> {
        if self.model.connection == Connection::Wired {
            return Ok(None);
        }
        Ok(protocol::format_serial(&self.get(DOMAIN_LOCAL, reg::SERIAL)?.payload))
    }

    pub fn dongle_firmware(&mut self) -> Result<Option<String>> {
        if self.model.connection == Connection::Wired {
            return Ok(None);
        }
        Ok(protocol::format_firmware(
            &self.get(DOMAIN_LOCAL, reg::FIRMWARE)?.payload,
        ))
    }

    // ----- power ----------------------------------------------------------

    pub fn battery(&mut self) -> Result<u8> {
        let level = self.headset_get_u8(reg::BATTERY)?;
        if level > 100 {
            return Err(Error::Protocol(format!("battery level out of range: {level}")));
        }
        Ok(level)
    }

    pub fn charging(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::CHARGING)? != 0)
    }

    /// Auto power-off after `minutes` of silence, 0 = never.
    pub fn sleep_minutes(&mut self) -> Result<u8> {
        self.headset_get_u8(reg::SLEEP_TIMER)
    }

    pub fn set_sleep_minutes(&mut self, minutes: u8) -> Result<()> {
        self.headset_set_u8(reg::SLEEP_TIMER, minutes)
    }

    // ----- microphone -----------------------------------------------------

    pub fn mic_muted(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::MIC_MUTE)? != 0)
    }

    pub fn sidetone_enabled(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::SIDETONE)? != 0)
    }

    pub fn set_sidetone_enabled(&mut self, on: bool) -> Result<()> {
        self.headset_set_u8(reg::SIDETONE, u8::from(on))
    }

    /// Sidetone volume 0..=15.
    pub fn sidetone_level(&mut self) -> Result<u8> {
        Ok(self.headset_get_u8(reg::SIDETONE_LEVEL)?.min(protocol::SIDETONE_MAX))
    }

    pub fn set_sidetone_level(&mut self, level: u8) -> Result<()> {
        if level > protocol::SIDETONE_MAX {
            return Err(Error::InvalidArgument(format!(
                "sidetone level must be 0..={}",
                protocol::SIDETONE_MAX
            )));
        }
        self.headset_set_u8(reg::SIDETONE_LEVEL, level)
    }

    // ----- misc -----------------------------------------------------------

    /// Block Bluetooth calls while the 2.4 GHz link is active.
    pub fn bt_dnd(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::BT_DND)? != 0)
    }

    pub fn set_bt_dnd(&mut self, on: bool) -> Result<()> {
        self.headset_set_u8(reg::BT_DND, u8::from(on))
    }

    pub fn led_mode(&mut self) -> Result<Option<LedMode>> {
        if self.model.connection == Connection::Wired {
            return Ok(None);
        }
        Ok(Some(LedMode::from_raw(self.get_u8(DOMAIN_LOCAL, reg::DONGLE_LED)?)))
    }

    pub fn set_led_mode(&mut self, mode: LedMode) -> Result<()> {
        if self.model.connection == Connection::Wired {
            return Err(Error::InvalidArgument(
                "the dongle LED can only be set through the dongle".into(),
            ));
        }
        self.set_u8(DOMAIN_LOCAL, reg::DONGLE_LED, mode.raw())
    }

    // ----- equalizer ------------------------------------------------------

    pub fn eq_preset(&mut self) -> Result<HardwarePreset> {
        Ok(HardwarePreset::from_raw(self.headset_get_u8(reg::EQ_PRESET)?))
    }

    /// Curve stored in the Custom slot, in dB (independent of the active preset).
    pub fn custom_eq(&mut self) -> Result<Bands> {
        let domain = self.headset_domain();
        let resp = self.get(domain, reg::EQ_BANDS)?;
        protocol::eq_from_payload(&resp.payload)
            .ok_or_else(|| Error::Protocol(format!("EQ reply has {} bytes", resp.payload.len())))
    }

    pub fn eq_enabled(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::EQ_ENABLE)? != 0)
    }

    pub fn set_eq_enabled(&mut self, on: bool) -> Result<()> {
        self.headset_set_u8(reg::EQ_ENABLE, u8::from(on))
    }

    /// "Audio enhancement" (a bass/spatial expander in the DSP).
    pub fn enhancement(&mut self) -> Result<bool> {
        Ok(self.headset_get_u8(reg::ENHANCEMENT)? != 0)
    }

    pub fn set_enhancement(&mut self, on: bool) -> Result<()> {
        self.headset_set_u8(reg::ENHANCEMENT, u8::from(on))
    }

    /// Select a preset slot and make sure the EQ stage is enabled.
    /// The enhancement flag is left as it is (selecting a preset does not
    /// change it; verified on hardware).
    pub fn select_preset(&mut self, preset: HardwarePreset) -> Result<()> {
        self.headset_set_u8(reg::EQ_PRESET, preset.raw())?;
        self.set_eq_enabled(true)
    }

    /// Write `bands` (dB) into the custom slot and make it the live curve.
    ///
    /// The order matters: the EQ stage must be enabled, the slot selected,
    /// the curve written and the slot re-selected so the DSP picks up the
    /// freshly written values.
    pub fn apply_custom_eq(&mut self, bands: &Bands) -> Result<()> {
        let wire = protocol::eq_to_wire(bands);
        self.set_eq_enabled(true)?;
        self.headset_set_u8(reg::EQ_PRESET, HardwarePreset::Custom.raw())?;
        let domain = self.headset_domain();
        self.transact(domain, reg::set(reg::EQ_BANDS), &wire)?;
        thread::sleep(Duration::from_millis(40));
        self.headset_set_u8(reg::EQ_PRESET, HardwarePreset::Custom.raw())
    }

    // ----- bulk -----------------------------------------------------------

    /// Read everything the device reports.
    pub fn read_state(&mut self) -> Result<HeadsetState> {
        let mut state = HeadsetState::default();
        self.refresh_dongle(&mut state)?;
        state.link_up = Some(self.link_up()?);
        if state.link_up == Some(true) {
            match self.refresh_headset(&mut state) {
                Ok(()) => {}
                Err(Error::HeadsetOffline) => {
                    state.clear_headset();
                    state.link_up = Some(false);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(state)
    }

    /// Dongle identity and LED. Unanswered registers are left as `None`.
    pub fn refresh_dongle(&mut self, state: &mut HeadsetState) -> Result<()> {
        if self.model.connection == Connection::Dongle {
            state.dongle_serial = optional(self.dongle_serial())?.flatten();
            state.dongle_firmware = optional(self.dongle_firmware())?.flatten();
            state.led_mode = optional(self.led_mode())?.flatten();
        }
        Ok(())
    }

    /// Every headset register. Fails with [`Error::HeadsetOffline`] when the
    /// headset stops answering altogether (powered off mid-way).
    pub fn refresh_headset(&mut self, state: &mut HeadsetState) -> Result<()> {
        state.headset_serial = self.try_read(Self::headset_serial)?.flatten();
        state.headset_firmware = self.try_read(Self::headset_firmware)?.flatten();
        self.refresh_status(state)?;
        state.sleep_minutes = self.try_read(Self::sleep_minutes)?;
        state.bt_dnd = self.try_read(Self::bt_dnd)?;
        state.sidetone_enabled = self.try_read(Self::sidetone_enabled)?;
        state.sidetone_level = self.try_read(Self::sidetone_level)?;
        state.eq_enabled = self.try_read(Self::eq_enabled)?;
        state.enhancement = self.try_read(Self::enhancement)?;
        self.refresh_eq(state)
    }

    /// Active preset and the Custom slot curve.
    pub fn refresh_eq(&mut self, state: &mut HeadsetState) -> Result<()> {
        state.eq_preset = self.try_read(Self::eq_preset)?;
        state.custom_eq = self.try_read(Self::custom_eq)?;
        Ok(())
    }

    /// The values that change on their own: battery, charging, mute button.
    pub fn refresh_status(&mut self, state: &mut HeadsetState) -> Result<()> {
        state.battery = self.try_read(Self::battery)?;
        state.charging = self.try_read(Self::charging)?;
        state.mic_muted = self.try_read(Self::mic_muted)?;
        Ok(())
    }

    /// Run one headset read. An unanswered register is `None` as long as the
    /// headset is still linked; otherwise the headset is reported offline.
    fn try_read<V>(&mut self, read: impl FnOnce(&mut Self) -> Result<V>) -> Result<Option<V>> {
        match read(self) {
            Ok(value) => Ok(Some(value)),
            // One odd value (e.g. battery 0xFF while booting) must not abort the whole refresh.
            Err(Error::Protocol(msg)) => {
                log::debug!("ignoring unexpected reply: {msg}");
                Ok(None)
            }
            Err(err) if err.is_timeout() => {
                if self.link_up()? {
                    log::debug!("headset did not answer, treating register as unsupported: {err}");
                    Ok(None)
                } else {
                    Err(Error::HeadsetOffline)
                }
            }
            Err(err) => Err(err),
        }
    }
}

/// Map "no answer" to `None`, keep real failures.
fn optional<V>(result: Result<V>) -> Result<Option<V>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.is_timeout() => {
            log::debug!("register unsupported: {err}");
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::sim::SimTransport;
    use super::*;
    use crate::devices::MODELS;

    fn dongle() -> BlackSharkV2Hs<SimTransport> {
        BlackSharkV2Hs::new(SimTransport::new(Connection::Dongle), &MODELS[0])
    }

    fn wired() -> BlackSharkV2Hs<SimTransport> {
        BlackSharkV2Hs::new(SimTransport::new(Connection::Wired), &MODELS[1])
    }

    #[test]
    fn reads_full_state_from_simulator() {
        let mut dev = dongle();
        let state = dev.read_state().unwrap();
        assert_eq!(state.link_up, Some(true));
        assert_eq!(state.battery, Some(76));
        assert_eq!(state.charging, Some(false));
        assert_eq!(state.eq_preset, Some(HardwarePreset::Music));
        assert_eq!(state.custom_eq, Some([2, 1, 0, 0, 0, 0, 0, 1, 2, 1]));
        assert_eq!(state.led_mode, Some(LedMode::Link));
        assert_eq!(state.headset_serial.as_deref(), Some("SIMHEADSET00001"));
        assert_eq!(state.dongle_serial.as_deref(), Some("SIMDONGLE000001"));
        assert_eq!(state.headset_firmware.as_deref(), Some("1.02.0.3"));
        assert_eq!(state.sidetone_level, Some(5));
    }

    #[test]
    fn setters_roundtrip() {
        let mut dev = dongle();
        dev.set_sidetone_enabled(true).unwrap();
        dev.set_sidetone_level(12).unwrap();
        dev.set_sleep_minutes(45).unwrap();
        dev.set_bt_dnd(true).unwrap();
        dev.set_led_mode(LedMode::Battery).unwrap();
        assert!(dev.sidetone_enabled().unwrap());
        assert_eq!(dev.sidetone_level().unwrap(), 12);
        assert_eq!(dev.sleep_minutes().unwrap(), 45);
        assert!(dev.bt_dnd().unwrap());
        assert_eq!(dev.led_mode().unwrap(), Some(LedMode::Battery));
        assert!(dev.set_sidetone_level(16).is_err());
    }

    #[test]
    fn custom_eq_sequence_and_offset() {
        let mut dev = dongle();
        let log = dev.transport.request_log();
        let curve = [-9, -3, 0, 1, 2, 3, 4, 5, 6, 0];
        dev.apply_custom_eq(&curve).unwrap();
        assert_eq!(dev.eq_preset().unwrap(), HardwarePreset::Custom);
        assert_eq!(dev.custom_eq().unwrap(), curve);

        let cmds: Vec<(u8, Vec<u8>)> = log.lock().unwrap().iter().map(|r| (r.cmd, r.params.clone())).collect();
        assert_eq!(
            cmds[..4],
            [
                (0x9E, vec![1]),
                (0x93, vec![0xFF]),
                (0x95, vec![0xFC, 2, 5, 6, 7, 8, 9, 10, 11, 5]),
                (0x93, vec![0xFF]),
            ]
        );
        // The enhancement flag is never touched as a side effect.
        assert!(log.lock().unwrap().iter().all(|r| r.cmd != 0x9D));
        assert!(log.lock().unwrap().iter().all(|r| r.domain == DOMAIN_HEADSET_RF));
    }

    #[test]
    fn rom_preset_selection() {
        let mut dev = dongle();
        dev.set_enhancement(true).unwrap();
        dev.select_preset(HardwarePreset::Game).unwrap();
        assert!(dev.enhancement().unwrap());
        assert_eq!(dev.eq_preset().unwrap(), HardwarePreset::Game);
        // Switching presets leaves the Custom slot untouched.
        assert_eq!(dev.custom_eq().unwrap(), [2, 1, 0, 0, 0, 0, 0, 1, 2, 1]);
        assert!(dev.eq_enabled().unwrap());
    }

    #[test]
    fn offline_headset_times_out_quickly_and_state_says_link_down() {
        let mut dev = dongle();
        dev.set_timeout(Duration::from_millis(30));
        dev.transport.set_link(false);
        assert!(matches!(dev.battery(), Err(Error::Timeout { cmd: 0x21, .. })));
        let state = dev.read_state().unwrap();
        assert_eq!(state.link_up, Some(false));
        assert_eq!(state.battery, None);
        // Dongle-local information is still available.
        assert_eq!(state.dongle_serial.as_deref(), Some("SIMDONGLE000001"));
    }

    #[test]
    fn wired_uses_local_domain_and_has_no_dongle() {
        let mut dev = wired();
        let log = dev.transport.request_log();
        let state = dev.read_state().unwrap();
        assert_eq!(state.link_up, Some(true));
        assert_eq!(state.dongle_serial, None);
        assert_eq!(state.led_mode, None);
        assert!(dev.set_led_mode(LedMode::Off).is_err());
        assert!(log.lock().unwrap().iter().all(|r| r.domain == DOMAIN_LOCAL));
    }

    #[test]
    fn foreign_frames_are_queued_not_consumed() {
        let mut dev = dongle();
        // A stale reply from another process and a media-key report arrive first.
        dev.transport
            .inject(protocol::encode_response(0x7E, DOMAIN_HEADSET_RF, reg::BATTERY, 1, &[42]).to_vec());
        dev.transport.inject(vec![0x0C, 0x01]);
        assert!(!dev.charging().unwrap());
        let events = dev.poll_incoming(Duration::ZERO).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Incoming::Frame(r) if r.cmd == reg::BATTERY && r.payload == [42]));
        assert_eq!(
            events[1],
            Incoming::Other {
                report_id: 0x0C,
                data: vec![0x01]
            }
        );
    }

    #[test]
    fn apply_reply_updates_state() {
        let mut state = HeadsetState::default();
        let reply = |domain, cmd, payload: &[u8]| {
            protocol::decode_response(&protocol::encode_response(0x60, domain, cmd, 1, payload)).unwrap()
        };
        assert!(state.apply_reply(&reply(DOMAIN_HEADSET_RF, reg::BATTERY, &[55]), Connection::Dongle));
        assert!(state.apply_reply(&reply(DOMAIN_HEADSET_RF, reg::EQ_PRESET, &[0x09]), Connection::Dongle));
        assert!(state.apply_reply(&reply(DOMAIN_LOCAL, reg::LINK_STATUS, &[1]), Connection::Dongle));
        assert!(state.apply_reply(&reply(DOMAIN_LOCAL, reg::SERIAL, b"DONGLE"), Connection::Dongle));
        // SET echoes and out-of-range values are ignored.
        assert!(!state.apply_reply(&reply(DOMAIN_HEADSET_RF, 0x98, &[1]), Connection::Dongle));
        assert!(!state.apply_reply(&reply(DOMAIN_HEADSET_RF, reg::BATTERY, &[200]), Connection::Dongle));
        // A dongle-local battery value does not describe the headset.
        assert!(!state.apply_reply(&reply(DOMAIN_LOCAL, reg::BATTERY, &[1]), Connection::Dongle));
        assert_eq!(state.battery, Some(55));
        assert_eq!(state.eq_preset, Some(HardwarePreset::Movie));
        assert_eq!(state.link_up, Some(true));
        assert_eq!(state.dongle_serial.as_deref(), Some("DONGLE"));
        assert_eq!(state.headset_serial, None);
    }

    #[test]
    fn transport_lock_key_enables_cross_process_lock() {
        let key = format!("test-sim-lock-{}", std::process::id());
        let mut dev = BlackSharkV2Hs::new(SimTransport::with_lock_key(Connection::Dongle, &key), &MODELS[0]);
        assert!(dev.lock.is_some());
        assert_eq!(dev.battery().unwrap(), 76);
        // While another holder keeps the lock, transactions wait and then fail.
        let other = DeviceLock::for_key(&key).unwrap();
        let _held = other.acquire(Duration::from_millis(100)).unwrap();
        let start = Instant::now();
        assert!(matches!(dev.battery(), Err(Error::Io(_))));
        assert!(start.elapsed() >= LOCK_TIMEOUT);
    }

    #[test]
    fn unsupported_register_does_not_mark_headset_offline() {
        let mut dev = dongle();
        dev.set_timeout(Duration::from_millis(30));
        // Unknown register: the simulated headset stays silent but is linked.
        assert_eq!(dev.try_read(|d| d.get_u8(DOMAIN_HEADSET_RF, 0x7A)).unwrap(), None);
        dev.transport.set_link(false);
        assert!(matches!(dev.try_read(|d| d.battery()), Err(Error::HeadsetOffline)));
    }
}
