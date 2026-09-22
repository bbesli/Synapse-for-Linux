use std::time::Duration;

use crate::Result;

/// A bidirectional HID report channel.
///
/// Reports are exchanged including their leading report ID byte, exactly
/// like `/dev/hidraw*` does for devices with numbered reports.
pub trait Transport: Send {
    /// Send one output report (first byte = report ID).
    fn write_report(&mut self, report: &[u8]) -> Result<()>;

    /// Wait up to `timeout` for one input report and copy it into `buf`.
    ///
    /// Returns `Ok(None)` when nothing arrived in time. A zero timeout only
    /// returns reports that are already queued.
    fn read_report(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>>;

    /// Name used to serialize access between processes (e.g. `hidraw3`).
    /// `None` disables cross-process locking.
    fn lock_key(&self) -> Option<String> {
        None
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        (**self).write_report(report)
    }

    fn read_report(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        (**self).read_report(buf, timeout)
    }

    fn lock_key(&self) -> Option<String> {
        (**self).lock_key()
    }
}
