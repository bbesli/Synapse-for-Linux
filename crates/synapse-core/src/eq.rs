//! Equalizer model: 10 bands, hardware preset slots and curves.

use serde::{Deserialize, Serialize};

pub const BAND_COUNT: usize = 10;

/// Center frequencies of the 10 hardware bands.
pub const BAND_FREQUENCIES_HZ: [u32; BAND_COUNT] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

/// Gain range the headset DSP actually applies (dB).
pub const MIN_GAIN_DB: i8 = -9;
pub const MAX_GAIN_DB: i8 = 6;

pub type Bands = [i8; BAND_COUNT];

pub const FLAT: Bands = [0; BAND_COUNT];

pub fn band_label(index: usize) -> String {
    let hz = BAND_FREQUENCIES_HZ[index];
    if hz >= 1000 {
        format!("{}k", hz / 1000)
    } else {
        hz.to_string()
    }
}

pub fn clamp_gain(db: i32) -> i8 {
    db.clamp(i32::from(MIN_GAIN_DB), i32::from(MAX_GAIN_DB)) as i8
}

pub fn clamp_bands(bands: Bands) -> Bands {
    bands.map(|b| clamp_gain(i32::from(b)))
}

/// Preset slot selected on the headset (register 0x13 / 0x93).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwarePreset {
    /// DSP bypass, 0 dB on every band.
    Flat,
    Game,
    Music,
    Movie,
    /// User curve stored in the headset's flash.
    Custom,
    /// A slot value this program does not know.
    Unknown(u8),
}

impl HardwarePreset {
    pub const SELECTABLE: [HardwarePreset; 5] = [
        HardwarePreset::Music,
        HardwarePreset::Game,
        HardwarePreset::Movie,
        HardwarePreset::Flat,
        HardwarePreset::Custom,
    ];

    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0x00 => Self::Flat,
            0x07 => Self::Game,
            0x08 => Self::Music,
            0x09 => Self::Movie,
            0xFF => Self::Custom,
            other => Self::Unknown(other),
        }
    }

    pub fn raw(self) -> u8 {
        match self {
            Self::Flat => 0x00,
            Self::Game => 0x07,
            Self::Music => 0x08,
            Self::Movie => 0x09,
            Self::Custom => 0xFF,
            Self::Unknown(raw) => raw,
        }
    }

    /// Stable identifier used by the CLI and the config file.
    pub fn key(self) -> String {
        match self {
            Self::Flat => "flat".into(),
            Self::Game => "game".into(),
            Self::Music => "music".into(),
            Self::Movie => "movie".into(),
            Self::Custom => "custom".into(),
            Self::Unknown(raw) => format!("0x{raw:02x}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "flat" | "off" | "bypass" | "default" => Some(Self::Flat),
            "game" => Some(Self::Game),
            "music" => Some(Self::Music),
            "movie" => Some(Self::Movie),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// Approximate factory curve, for display only. The real curve lives in
    /// the headset ROM and cannot be read back (register 0x15 returns the
    /// Custom slot).
    pub fn reference_curve(self) -> Option<Bands> {
        match self {
            Self::Flat => Some(FLAT),
            Self::Music => Some([3, 2, 1, 0, 0, 0, 1, 2, 3, 2]),
            Self::Game => Some([-8, -6, -4, -4, 3, 4, -3, -4, -4, -4]),
            Self::Movie => Some([4, 2, 0, -2, 0, 2, 4, 3, 2, 1]),
            Self::Custom | Self::Unknown(_) => None,
        }
    }

    pub fn is_rom(self) -> bool {
        matches!(self, Self::Flat | Self::Game | Self::Music | Self::Movie)
    }
}

/// Curves shipped with the application, written to the custom slot.
pub struct BuiltinCurve {
    pub key: &'static str,
    pub name_en: &'static str,
    pub name_tr: &'static str,
    pub bands: Bands,
}

pub const BUILTIN_CURVES: &[BuiltinCurve] = &[
    BuiltinCurve {
        key: "bass",
        name_en: "Bass Boost",
        name_tr: "Bas Güçlendirme",
        bands: [5, 4, 2, -1, -1, 0, 0, 1, 1, 1],
    },
    BuiltinCurve {
        key: "footsteps",
        name_en: "FPS Footsteps",
        name_tr: "FPS Ayak Sesi",
        bands: [-6, -5, -3, -1, 1, 3, 5, 5, 3, 0],
    },
    BuiltinCurve {
        key: "voice",
        name_en: "Voice",
        name_tr: "Vokal",
        bands: [-3, -2, -1, 1, 3, 4, 4, 3, 1, 0],
    },
    BuiltinCurve {
        key: "treble",
        name_en: "Treble Boost",
        name_tr: "Tiz Güçlendirme",
        bands: [0, 0, 0, 0, 0, 1, 2, 4, 5, 5],
    },
];

pub fn builtin_curve(key: &str) -> Option<&'static BuiltinCurve> {
    BUILTIN_CURVES.iter().find(|c| c.key.eq_ignore_ascii_case(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_raw_roundtrip() {
        for p in HardwarePreset::SELECTABLE {
            assert_eq!(HardwarePreset::from_raw(p.raw()), p);
            assert_eq!(HardwarePreset::from_key(&p.key()), Some(p));
        }
        assert_eq!(HardwarePreset::from_raw(0x42), HardwarePreset::Unknown(0x42));
        assert_eq!(HardwarePreset::Unknown(0x42).raw(), 0x42);
    }

    #[test]
    fn curves_are_within_hardware_range() {
        let all = BUILTIN_CURVES
            .iter()
            .map(|c| c.bands)
            .chain(HardwarePreset::SELECTABLE.iter().filter_map(|p| p.reference_curve()));
        for bands in all {
            assert!(
                bands.iter().all(|b| (MIN_GAIN_DB..=MAX_GAIN_DB).contains(b)),
                "{bands:?}"
            );
        }
    }

    #[test]
    fn labels_and_clamping() {
        assert_eq!(band_label(0), "31");
        assert_eq!(band_label(5), "1k");
        assert_eq!(band_label(9), "16k");
        assert_eq!(clamp_gain(-20), -9);
        assert_eq!(clamp_gain(20), 6);
        assert_eq!(
            clamp_bands([-12, 9, 0, 0, 0, 0, 0, 0, 0, 3]),
            [-9, 6, 0, 0, 0, 0, 0, 0, 0, 3]
        );
    }
}
