//! `/dev/hidraw*` transport.
//!
//! Output reports are sent with `write(2)`; the kernel forwards them to the
//! device (falling back to a SET_REPORT control transfer when the interface
//! has no interrupt OUT endpoint, which is the case for the Razer dongles).
//! Input reports are received with `poll(2)` + `read(2)`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::Transport;
use crate::{Error, Result};

pub struct Hidraw {
    file: File,
    path: PathBuf,
}

impl Hidraw {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|err| match err.kind() {
                io::ErrorKind::PermissionDenied => Error::PermissionDenied { path: path.clone() },
                io::ErrorKind::NotFound => Error::Disconnected,
                _ => Error::from(err),
            })?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait until the descriptor is readable. Returns `false` on timeout.
    fn wait_readable(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            // Round up so that a sub-millisecond remainder still waits.
            let ms = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
            let mut pfd = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a valid pollfd for the duration of the call.
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if rc == 0 {
                return Ok(false);
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 && pfd.revents & libc::POLLIN == 0 {
                return Err(Error::Disconnected);
            }
            return Ok(true);
        }
    }
}

impl Transport for Hidraw {
    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        loop {
            match self.file.write(report) {
                Ok(n) if n == report.len() => return Ok(()),
                Ok(n) => {
                    return Err(Error::Protocol(format!(
                        "short write to {}: {n} of {} bytes",
                        self.path.display(),
                        report.len()
                    )));
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn read_report(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        loop {
            match self.file.read(buf) {
                // hidraw never returns 0 for a live device; treat it as a hang-up.
                Ok(0) => return Err(Error::Disconnected),
                Ok(n) => return Ok(Some(n)),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if timeout.is_zero() || !self.wait_readable(timeout)? {
                        return Ok(None);
                    }
                    // Readable now: loop around and read it. A spurious wakeup
                    // simply ends in WouldBlock + another (shorter) wait.
                    return match self.file.read(buf) {
                        Ok(0) => Err(Error::Disconnected),
                        Ok(n) => Ok(Some(n)),
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
                        Err(err) => Err(err.into()),
                    };
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn lock_key(&self) -> Option<String> {
        self.path.file_name().map(|name| name.to_string_lossy().into_owned())
    }
}
