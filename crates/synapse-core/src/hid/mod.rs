//! Raw HID plumbing: sysfs enumeration, report descriptor parsing,
//! the `/dev/hidraw*` transport and cross-process locking.

pub mod descriptor;
pub mod hidraw;
pub mod lock;
pub mod sysfs;
pub mod transport;

pub use transport::Transport;
