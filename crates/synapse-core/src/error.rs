use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Errors produced while talking to a device.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no supported Razer device found")]
    NoDevice,

    #[error("permission denied opening {path} (the udev rule is not installed, see README)")]
    PermissionDenied { path: PathBuf },

    #[error("device disconnected")]
    Disconnected,

    #[error("no response to command 0x{cmd:02X} within {timeout:?}")]
    Timeout { cmd: u8, timeout: Duration },

    #[error("empty response to command 0x{cmd:02X}")]
    EmptyResponse { cmd: u8 },

    #[error("the headset is not connected to the dongle (powered off or out of range)")]
    HeadsetOffline,

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error(transparent)]
    Io(io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        match err.raw_os_error() {
            // The device node vanished (unplugged) or the USB link went down.
            // hidraw's read() reports a removed device as EIO, write() as ENODEV.
            Some(libc::ENODEV | libc::ESHUTDOWN | libc::ENXIO | libc::EIO) => Error::Disconnected,
            _ => Error::Io(err),
        }
    }
}

impl Error {
    /// True when retrying later (e.g. after re-plugging) could help.
    pub fn is_disconnect(&self) -> bool {
        matches!(self, Error::Disconnected)
    }

    /// True when the command simply went unanswered.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout { .. } | Error::EmptyResponse { .. })
    }
}
