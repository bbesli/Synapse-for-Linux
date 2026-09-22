//! In-memory BlackShark V2 HyperSpeed used for tests and for trying the
//! applications without hardware (`SYNAPSE_LINUX_SIMULATE=1`).
//!
//! It answers like the real device does: replies echo the sequence byte and
//! command, EQ gains are stored with the -5 dB write offset, register 0x15
//! returns the Custom slot whatever preset is active, and nothing answers on
//! the RF domain while the headset link is down.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::protocol::{self, DOMAIN_HEADSET_RF, DOMAIN_LOCAL, EQ_WIRE_OFFSET, Request, SIDETONE_MAX, reg};
use crate::devices::Connection;
use crate::eq::{self, Bands, HardwarePreset};
use crate::hid::Transport;
use crate::{Error, Result};

#[derive(Debug, Clone)]
struct Registers {
    headset_serial: &'static str,
    dongle_serial: &'static str,
    headset_fw: [u8; 4],
    dongle_fw: [u8; 4],
    battery: u8,
    charging: u8,
    sleep: u8,
    dnd: u8,
    mic_mute: u8,
    led: u8,
    sidetone: u8,
    sidetone_level: u8,
    preset: u8,
    custom: Bands,
    eq_enable: u8,
    enhancement: u8,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            headset_serial: "SIMHEADSET00001",
            dongle_serial: "SIMDONGLE000001",
            headset_fw: [1, 2, 0, 3],
            dongle_fw: [1, 0, 4, 0],
            battery: 76,
            charging: 0,
            sleep: 15,
            dnd: 0,
            mic_mute: 0,
            led: 1,
            sidetone: 0,
            sidetone_level: 5,
            preset: HardwarePreset::Music.raw(),
            custom: [2, 1, 0, 0, 0, 0, 0, 1, 2, 1],
            eq_enable: 1,
            enhancement: 0,
        }
    }
}

pub struct SimTransport {
    connection: Connection,
    regs: Registers,
    link_up: bool,
    replies: VecDeque<Vec<u8>>,
    requests: Arc<Mutex<Vec<Request>>>,
    lock_key: Option<String>,
}

impl SimTransport {
    pub fn new(connection: Connection) -> Self {
        let mut regs = Registers::default();
        if let Some(level) = std::env::var("SYNAPSE_LINUX_SIM_BATTERY")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
        {
            regs.battery = level.min(100);
        }
        Self {
            connection,
            regs,
            link_up: std::env::var("SYNAPSE_LINUX_SIM_OFFLINE").is_err(),
            replies: VecDeque::new(),
            requests: Arc::new(Mutex::new(Vec::new())),
            lock_key: None,
        }
    }

    pub fn with_lock_key(connection: Connection, key: &str) -> Self {
        Self {
            lock_key: Some(key.to_string()),
            ..Self::new(connection)
        }
    }

    /// Power the simulated headset on/off (dongle only).
    pub fn set_link(&mut self, up: bool) {
        self.link_up = up;
    }

    /// Queue a raw report as if the device had sent it.
    pub fn inject(&mut self, report: Vec<u8>) {
        self.replies.push_back(report);
    }

    /// Every request received so far (shared handle, for tests).
    pub fn request_log(&self) -> Arc<Mutex<Vec<Request>>> {
        Arc::clone(&self.requests)
    }

    fn headset_reachable(&self, domain: u8) -> bool {
        match self.connection {
            Connection::Wired => domain == DOMAIN_LOCAL,
            Connection::Dongle => domain == DOMAIN_HEADSET_RF && self.link_up,
        }
    }

    /// Returns the reply payload, or `None` when the device stays silent.
    fn handle(&mut self, req: &Request) -> Option<Vec<u8>> {
        let local_dongle = self.connection == Connection::Dongle && req.domain == DOMAIN_LOCAL;
        let reachable = self.headset_reachable(req.domain);
        let link_up = self.link_up;
        let arg = req.params.first().copied();
        let r = &mut self.regs;

        // Registers the dongle answers itself.
        if local_dongle {
            return match req.cmd {
                reg::SERIAL => Some(r.dongle_serial.as_bytes().to_vec()),
                reg::FIRMWARE => Some(r.dongle_fw.to_vec()),
                reg::USB_PID => Some(vec![0x05, 0x65]),
                reg::LINK_STATUS => Some(vec![u8::from(link_up)]),
                reg::DONGLE_LED => Some(vec![r.led]),
                c if c == reg::set(reg::DONGLE_LED) => {
                    r.led = arg?.min(3);
                    Some(vec![])
                }
                _ => None,
            };
        }
        if !reachable {
            return None;
        }

        let set = |value: &mut u8, v: Option<u8>| -> Option<Vec<u8>> {
            *value = v?;
            Some(vec![])
        };
        match req.cmd {
            reg::SERIAL => Some(r.headset_serial.as_bytes().to_vec()),
            reg::FIRMWARE => Some(r.headset_fw.to_vec()),
            reg::USB_PID => Some(vec![0x05, 0x6E]),
            reg::LINK_STATUS => Some(vec![1]),
            reg::BATTERY => Some(vec![r.battery]),
            reg::CHARGING => Some(vec![r.charging]),
            reg::SLEEP_TIMER => Some(vec![r.sleep]),
            reg::BT_DND => Some(vec![r.dnd]),
            reg::MIC_MUTE => Some(vec![r.mic_mute]),
            reg::SIDETONE => Some(vec![r.sidetone]),
            reg::SIDETONE_LEVEL => Some(vec![r.sidetone_level]),
            reg::EQ_PRESET => Some(vec![r.preset]),
            reg::EQ_ENABLE => Some(vec![r.eq_enable]),
            reg::ENHANCEMENT => Some(vec![r.enhancement]),
            // The real headset returns the Custom slot whatever preset is active.
            reg::EQ_BANDS => Some(r.custom.iter().map(|&b| b as u8).collect()),
            c if c == reg::set(reg::SLEEP_TIMER) => set(&mut r.sleep, arg),
            c if c == reg::set(reg::BT_DND) => set(&mut r.dnd, arg.map(|v| u8::from(v != 0))),
            c if c == reg::set(reg::SIDETONE) => set(&mut r.sidetone, arg.map(|v| u8::from(v != 0))),
            c if c == reg::set(reg::SIDETONE_LEVEL) => set(&mut r.sidetone_level, arg.map(|v| v.min(SIDETONE_MAX))),
            c if c == reg::set(reg::EQ_ENABLE) => set(&mut r.eq_enable, arg.map(|v| u8::from(v != 0))),
            c if c == reg::set(reg::ENHANCEMENT) => set(&mut r.enhancement, arg.map(|v| u8::from(v != 0))),
            c if c == reg::set(reg::EQ_PRESET) => {
                let preset = HardwarePreset::from_raw(arg?);
                if matches!(preset, HardwarePreset::Unknown(_)) {
                    return Some(vec![]); // ignored by the firmware
                }
                r.preset = preset.raw();
                Some(vec![])
            }
            c if c == reg::set(reg::EQ_BANDS) => {
                if req.params.len() < eq::BAND_COUNT {
                    return None;
                }
                for (band, &wire) in r.custom.iter_mut().zip(&req.params) {
                    *band = eq::clamp_gain(i32::from(wire as i8) - i32::from(EQ_WIRE_OFFSET));
                }
                Some(vec![])
            }
            _ => None,
        }
    }
}

impl Transport for SimTransport {
    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        let req = protocol::decode_request(report).map_err(|e| Error::Protocol(format!("simulator: {e}")))?;
        self.requests.lock().expect("request log").push(req.clone());
        if let Some(payload) = self.handle(&req) {
            self.replies
                .push_back(protocol::encode_response(req.seq, req.domain, req.cmd, 0x01, &payload).to_vec());
        }
        Ok(())
    }

    fn read_report(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        match self.replies.pop_front() {
            Some(report) => {
                let n = report.len().min(buf.len());
                buf[..n].copy_from_slice(&report[..n]);
                Ok(Some(n))
            }
            None => {
                // Nothing will ever arrive on its own; honour the wait like real I/O would.
                if !timeout.is_zero() {
                    thread::sleep(timeout.min(Duration::from_millis(50)));
                }
                Ok(None)
            }
        }
    }

    fn lock_key(&self) -> Option<String> {
        self.lock_key.clone()
    }
}
