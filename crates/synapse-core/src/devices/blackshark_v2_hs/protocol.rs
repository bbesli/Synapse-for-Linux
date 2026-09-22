//! Wire format of the MediaTek based HyperSpeed headsets
//! (BlackShark V2 HyperSpeed dongle 1532:0565, wired 1532:056E).
//!
//! Every exchange is a 64 byte HID report with report ID 0x02 on the vendor
//! collection (usage page 0xFF14). Offsets below are into the full report
//! as seen by hidraw, i.e. including the report ID at index 0:
//!
//! ```text
//! [0]      0x02        report ID
//! [1]      0x00        reserved / status
//! [2]      0x60 | seq  client mask + 5 bit rolling sequence number
//! [3..6]   0x00        reserved
//! [6]      len         4 + number of parameter bytes
//! [7]      0x00        reserved
//! [8]      dir         0x00 = host -> device
//! [9]      domain      0x80 = relay to the headset over 2.4 GHz, 0x00 = local (dongle / wired headset)
//! [10]     cmd         register; SET = GET | 0x80
//! [11]     status      0x00 in requests
//! [12]     count       number of parameter bytes
//! [13..]   params
//! [62]     checksum    XOR of bytes [0..62] (requests include the report ID)
//! [63]     0x00
//! ```
//!
//! Protocol knowledge from the MIT licensed reverse engineering work at
//! <https://github.com/justik13/razer-blackshark-v2-hyperspeed-webhid>.

use std::fmt;

pub const REPORT_ID: u8 = 0x02;
pub const REPORT_LEN: usize = 64;
pub const MAX_PARAMS: usize = 48;
pub const SEQ_CLIENT_MASK: u8 = 0x60;
pub const SEQ_MASK: u8 = 0x1F;

pub const DOMAIN_HEADSET_RF: u8 = 0x80;
pub const DOMAIN_LOCAL: u8 = 0x00;

const OFF_SEQ: usize = 2;
const OFF_LEN: usize = 6;
const OFF_DIR: usize = 8;
const OFF_DOMAIN: usize = 9;
const OFF_CMD: usize = 10;
const OFF_STATUS: usize = 11;
const OFF_COUNT: usize = 12;
const OFF_PARAMS: usize = 13;
const OFF_CHECKSUM: usize = 62;

/// Register numbers. The SET variant of a register is `GET | 0x80`.
pub mod reg {
    pub const SERIAL: u8 = 0x00;
    pub const FIRMWARE: u8 = 0x02;
    pub const USB_PID: u8 = 0x03;
    pub const EQ_PRESET: u8 = 0x13;
    pub const EQ_BANDS: u8 = 0x15;
    pub const SIDETONE: u8 = 0x18;
    pub const SIDETONE_LEVEL: u8 = 0x19;
    pub const ENHANCEMENT: u8 = 0x1D;
    pub const EQ_ENABLE: u8 = 0x1E;
    pub const LINK_STATUS: u8 = 0x20;
    pub const BATTERY: u8 = 0x21;
    pub const BT_DND: u8 = 0x27;
    pub const CHARGING: u8 = 0x2A;
    pub const SLEEP_TIMER: u8 = 0x2C;
    pub const MIC_MUTE: u8 = 0x55;
    pub const DONGLE_LED: u8 = 0x66;

    pub const SET: u8 = 0x80;

    pub const fn set(get: u8) -> u8 {
        get | SET
    }

    pub const fn is_set(cmd: u8) -> bool {
        cmd & SET != 0
    }

    pub const fn get_of(cmd: u8) -> u8 {
        cmd & !SET
    }
}

/// Sidetone level range accepted by the headset DSP.
pub const SIDETONE_MAX: u8 = 15;
/// Offset the DSP subtracts from written EQ gains (`stored = wire - 5`).
pub const EQ_WIRE_OFFSET: i8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub seq: u8,
    pub domain: u8,
    pub cmd: u8,
    pub params: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub seq: u8,
    pub dir: u8,
    pub domain: u8,
    pub cmd: u8,
    pub status: u8,
    pub payload: Vec<u8>,
}

impl Response {
    /// Does this frame answer a request with this sequence byte and command?
    pub fn answers(&self, seq: u8, cmd: u8) -> bool {
        self.cmd == cmd && (self.seq & SEQ_MASK) == (seq & SEQ_MASK)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not a report 0x02 frame (e.g. media keys on report 0x0C).
    OtherReport(u8),
    TooShort(usize),
    Checksum {
        expected: u8,
        found: u8,
    },
    BadCount(usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::OtherReport(id) => write!(f, "report 0x{id:02X} is not a control frame"),
            DecodeError::TooShort(n) => write!(f, "frame too short ({n} bytes)"),
            DecodeError::Checksum { expected, found } => {
                write!(f, "checksum mismatch (expected 0x{expected:02X}, found 0x{found:02X})")
            }
            DecodeError::BadCount(n) => write!(f, "invalid parameter count {n}"),
        }
    }
}

fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, b| acc ^ b)
}

pub fn sequence_byte(counter: u8) -> u8 {
    SEQ_CLIENT_MASK | (counter & SEQ_MASK)
}

/// Build a host -> device frame. `seq` is the full sequence byte (see [`sequence_byte`]).
pub fn encode_request(seq: u8, domain: u8, cmd: u8, params: &[u8]) -> Result<[u8; REPORT_LEN], String> {
    if params.len() > MAX_PARAMS {
        return Err(format!("too many parameters ({} > {MAX_PARAMS})", params.len()));
    }
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[OFF_SEQ] = seq;
    buf[OFF_LEN] = (4 + params.len()) as u8;
    buf[OFF_DIR] = 0x00;
    buf[OFF_DOMAIN] = domain;
    buf[OFF_CMD] = cmd;
    buf[OFF_STATUS] = 0x00;
    buf[OFF_COUNT] = params.len() as u8;
    buf[OFF_PARAMS..OFF_PARAMS + params.len()].copy_from_slice(params);
    buf[OFF_CHECKSUM] = xor(&buf[..OFF_CHECKSUM]);
    Ok(buf)
}

/// Parse a host -> device frame (used by the simulator and tests).
pub fn decode_request(report: &[u8]) -> Result<Request, DecodeError> {
    let (seq, _dir, domain, cmd, _status, params) = split_frame(report, true)?;
    Ok(Request {
        seq,
        domain,
        cmd,
        params,
    })
}

/// Build a device -> host frame (used by the simulator and tests).
pub fn encode_response(seq: u8, domain: u8, cmd: u8, status: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
    let payload = &payload[..payload.len().min(MAX_PARAMS)];
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[OFF_SEQ] = seq;
    buf[OFF_LEN] = (4 + payload.len()) as u8;
    buf[OFF_DIR] = 0x80;
    buf[OFF_DOMAIN] = domain;
    buf[OFF_CMD] = cmd;
    buf[OFF_STATUS] = status;
    buf[OFF_COUNT] = payload.len() as u8;
    buf[OFF_PARAMS..OFF_PARAMS + payload.len()].copy_from_slice(payload);
    // Replies are checksummed without the report ID byte.
    buf[OFF_CHECKSUM] = xor(&buf[1..OFF_CHECKSUM]);
    buf
}

/// Parse a device -> host frame as read from hidraw (report ID included).
pub fn decode_response(report: &[u8]) -> Result<Response, DecodeError> {
    let (seq, dir, domain, cmd, status, payload) = split_frame(report, false)?;
    Ok(Response {
        seq,
        dir,
        domain,
        cmd,
        status,
        payload,
    })
}

type Fields = (u8, u8, u8, u8, u8, Vec<u8>);

fn split_frame(report: &[u8], request: bool) -> Result<Fields, DecodeError> {
    let Some(&id) = report.first() else {
        return Err(DecodeError::TooShort(0));
    };
    if id != REPORT_ID {
        return Err(DecodeError::OtherReport(id));
    }
    if report.len() < OFF_CHECKSUM + 1 {
        return Err(DecodeError::TooShort(report.len()));
    }
    let found = report[OFF_CHECKSUM];
    let with_id = xor(&report[..OFF_CHECKSUM]);
    let without_id = with_id ^ REPORT_ID;
    // Requests always include the report ID. Replies are documented without
    // it; accept both variants for replies so firmware differences don't
    // break communication.
    let ok = if request {
        found == with_id
    } else {
        found == without_id || found == with_id
    };
    if !ok {
        let expected = if request { with_id } else { without_id };
        return Err(DecodeError::Checksum { expected, found });
    }
    let count = report[OFF_COUNT] as usize;
    if OFF_PARAMS + count > OFF_CHECKSUM {
        return Err(DecodeError::BadCount(count));
    }
    Ok((
        report[OFF_SEQ],
        report[OFF_DIR],
        report[OFF_DOMAIN],
        report[OFF_CMD],
        report[OFF_STATUS],
        report[OFF_PARAMS..OFF_PARAMS + count].to_vec(),
    ))
}

/// Convert gains in dB to the bytes written with register 0x95.
pub fn eq_to_wire(bands: &crate::eq::Bands) -> [u8; crate::eq::BAND_COUNT] {
    crate::eq::clamp_bands(*bands).map(|db| (db + EQ_WIRE_OFFSET) as u8)
}

/// Convert bytes read with register 0x15 (already in dB) to gains.
pub fn eq_from_payload(payload: &[u8]) -> Option<crate::eq::Bands> {
    if payload.len() < crate::eq::BAND_COUNT {
        return None;
    }
    let mut bands = [0i8; crate::eq::BAND_COUNT];
    for (band, &raw) in bands.iter_mut().zip(payload) {
        *band = crate::eq::clamp_gain(i32::from(raw as i8));
    }
    Some(bands)
}

/// Firmware version bytes -> `1.02.3.4`.
pub fn format_firmware(payload: &[u8]) -> Option<String> {
    match payload {
        [major, minor, build, rev, ..] => Some(format!("{major}.{minor:02}.{build}.{rev}")),
        [major, minor, ..] => Some(format!("{major}.{minor:02}")),
        _ => None,
    }
}

/// Serial number bytes -> trimmed ASCII string.
pub fn format_serial(payload: &[u8]) -> Option<String> {
    let text: String = payload
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| b as char)
        .collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_query_layout() {
        let frame = encode_request(0x65, DOMAIN_HEADSET_RF, reg::BATTERY, &[]).unwrap();
        let mut expected = [0u8; 64];
        expected[0] = 0x02;
        expected[2] = 0x65;
        expected[6] = 0x04;
        expected[9] = 0x80;
        expected[10] = 0x21;
        expected[62] = 0x02 ^ 0x65 ^ 0x04 ^ 0x80 ^ 0x21;
        assert_eq!(frame, expected);
    }

    #[test]
    fn set_with_parameter_layout() {
        let frame = encode_request(0x61, DOMAIN_LOCAL, reg::set(reg::DONGLE_LED), &[2]).unwrap();
        assert_eq!(frame[6], 0x05);
        assert_eq!(frame[9], 0x00);
        assert_eq!(frame[10], 0xE6);
        assert_eq!(frame[12], 1);
        assert_eq!(frame[13], 2);
        assert_eq!(frame[62], frame[..62].iter().fold(0, |a, b| a ^ b));
        assert_eq!(frame[63], 0);
    }

    /// Cross-check against the reference implementation: its 63 byte payload
    /// (without report ID) is checksummed as `0x02 ^ p[0..61]`.
    #[test]
    fn checksum_matches_reference_formula() {
        let frame = encode_request(
            0x7F,
            DOMAIN_HEADSET_RF,
            reg::set(reg::EQ_BANDS),
            &[8, 7, 6, 5, 5, 5, 6, 7, 8, 7],
        )
        .unwrap();
        let payload = &frame[1..];
        let reference = payload[..61].iter().fold(0x02u8, |a, b| a ^ b);
        assert_eq!(payload[61], reference);
        assert_eq!(payload[5], 0x0E); // length for 10 parameter bytes
    }

    #[test]
    fn request_roundtrip() {
        let frame = encode_request(0x6A, DOMAIN_HEADSET_RF, 0x99, &[12]).unwrap();
        let req = decode_request(&frame).unwrap();
        assert_eq!(
            req,
            Request {
                seq: 0x6A,
                domain: 0x80,
                cmd: 0x99,
                params: vec![12]
            }
        );
        let mut broken = frame;
        broken[13] ^= 1;
        assert!(matches!(decode_request(&broken), Err(DecodeError::Checksum { .. })));
    }

    #[test]
    fn response_roundtrip_and_both_checksum_variants() {
        let frame = encode_response(0x65, DOMAIN_HEADSET_RF, reg::BATTERY, 0x01, &[85]);
        let resp = decode_response(&frame).unwrap();
        assert_eq!(resp.payload, vec![85]);
        assert_eq!(resp.status, 0x01);
        assert!(resp.answers(0x65, reg::BATTERY));
        assert!(resp.answers(0x05, reg::BATTERY)); // mask bits are not compared
        assert!(!resp.answers(0x66, reg::BATTERY));
        assert!(!resp.answers(0x65, reg::CHARGING));

        // Same frame checksummed including the report ID is accepted too.
        let mut alt = frame;
        alt[62] ^= REPORT_ID;
        assert!(decode_response(&alt).is_ok());

        let mut broken = frame;
        broken[62] ^= 0x10;
        assert!(matches!(decode_response(&broken), Err(DecodeError::Checksum { .. })));
    }

    #[test]
    fn rejects_foreign_and_malformed_reports() {
        assert_eq!(decode_response(&[0x0C, 0x01]), Err(DecodeError::OtherReport(0x0C)));
        assert_eq!(decode_response(&[]), Err(DecodeError::TooShort(0)));
        assert_eq!(decode_response(&[0x02; 10]), Err(DecodeError::TooShort(10)));
        let mut frame = encode_response(0x60, 0, 0x21, 1, &[]);
        frame[12] = 60; // count runs over the checksum
        frame[62] = frame[1..62].iter().fold(0, |a, b| a ^ b);
        assert_eq!(decode_response(&frame), Err(DecodeError::BadCount(60)));
    }

    #[test]
    fn too_many_params_is_an_error() {
        assert!(encode_request(0x60, 0, 0x95, &[0; 49]).is_err());
        assert!(encode_request(0x60, 0, 0x95, &[0; 48]).is_ok());
    }

    #[test]
    fn eq_wire_conversion() {
        assert_eq!(
            eq_to_wire(&[3, 2, 1, 0, 0, 0, 1, 2, 3, 2]),
            [8, 7, 6, 5, 5, 5, 6, 7, 8, 7]
        );
        // -9 dB goes out as -4 (0xFC); out of range values are clamped first.
        assert_eq!(
            eq_to_wire(&[-9, 6, -12, 10, 0, 0, 0, 0, 0, 0])[..4],
            [0xFC, 11, 0xFC, 11]
        );
        assert_eq!(
            eq_from_payload(&[0xF7, 6, 0, 0xFF, 1, 2, 3, 4, 5, 250]),
            Some([-9, 6, 0, -1, 1, 2, 3, 4, 5, -6])
        );
        assert_eq!(eq_from_payload(&[1, 2, 3]), None);
    }

    #[test]
    fn formats_identity_fields() {
        assert_eq!(format_firmware(&[1, 2, 3, 4]).as_deref(), Some("1.02.3.4"));
        assert_eq!(format_firmware(&[1]), None);
        assert_eq!(
            format_serial(b"PM2345H01234567\0\0").as_deref(),
            Some("PM2345H01234567")
        );
        assert_eq!(format_serial(&[0, 0, 0]), None);
    }

    #[test]
    fn register_helpers() {
        assert_eq!(reg::set(reg::SIDETONE), 0x98);
        assert_eq!(reg::set(reg::EQ_BANDS), 0x95);
        assert_eq!(reg::set(reg::SLEEP_TIMER), 0xAC);
        assert!(reg::is_set(0xE6));
        assert_eq!(reg::get_of(0xE6), reg::DONGLE_LED);
        assert_eq!(sequence_byte(0x25), 0x65);
    }
}
