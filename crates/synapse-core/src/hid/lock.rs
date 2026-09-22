//! Cross-process serialization of device transactions.
//!
//! Every process that opens a hidraw node receives a copy of every input
//! report. The GUI, the tray and `synapsectl` may run at the same time, so a
//! request/response exchange is wrapped in an exclusive `flock(2)` on a small
//! lock file in `$XDG_RUNTIME_DIR`. The kernel drops the lock automatically if
//! a process dies.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub struct DeviceLock {
    file: Arc<File>,
}

/// Held while a transaction is in flight; unlocks on drop.
pub struct LockGuard {
    file: Arc<File>,
}

impl DeviceLock {
    pub fn for_key(key: &str) -> io::Result<Self> {
        let dir = lock_dir();
        std::fs::create_dir_all(&dir)?;
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(format!("{safe}.lock")))?;
        Ok(Self { file: Arc::new(file) })
    }

    /// Acquire the lock, waiting at most `timeout`.
    pub fn acquire(&self, timeout: Duration) -> io::Result<LockGuard> {
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: plain flock on a descriptor we own.
            let rc = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(LockGuard {
                    file: Arc::clone(&self.file),
                });
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EWOULDBLOCK) | Some(libc::EINTR) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(3));
                }
                Some(libc::EWOULDBLOCK) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "device is busy (locked by another process)",
                    ));
                }
                _ => return Err(err),
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // SAFETY: unlocking a lock we hold.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("synapse-linux"),
        // SAFETY: getuid never fails.
        _ => std::env::temp_dir().join(format!("synapse-linux-{}", unsafe { libc::getuid() })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_times_out_while_held() {
        let key = format!("test-{}", std::process::id());
        let a = DeviceLock::for_key(&key).unwrap();
        let b = DeviceLock::for_key(&key).unwrap();
        let guard = a.acquire(Duration::from_millis(50)).unwrap();
        let err = b.acquire(Duration::from_millis(30)).err().expect("must be busy");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        drop(guard);
        b.acquire(Duration::from_millis(50)).expect("free again");
    }
}
