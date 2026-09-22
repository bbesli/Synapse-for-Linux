//! Process-level helpers: single instance locks, launching the other mode
//! of this binary, and the login autostart entry for the tray.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const AUTOSTART_FILE: &str = "synapse-linux-tray.desktop";

fn runtime_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        // SAFETY: getuid never fails.
        _ => std::env::temp_dir().join(format!("synapse-linux-{}", unsafe { libc::getuid() })),
    };
    base.join("synapse-linux")
}

/// Hold an exclusive lock named `name` for the life of the returned file.
/// `Ok(None)` when another process already holds it; `Err` when the lock
/// file itself is unusable (e.g. a root-owned runtime directory).
pub fn single_instance(name: &str) -> io::Result<Option<File>> {
    let dir = runtime_dir();
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(format!("{name}.instance")))?;
    // `instance_running` probes by taking the lock for an instant; retry
    // briefly so a probe racing our start-up does not look like a rival.
    for attempt in 0..5 {
        // SAFETY: flock on a descriptor we own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(err);
        }
        if attempt < 4 {
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }
    Ok(None)
}

/// Is another process holding the `name` lock?
pub fn instance_running(name: &str) -> bool {
    let path = runtime_dir().join(format!("{name}.instance"));
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    // SAFETY: flock on a descriptor we own; released when `file` drops.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    rc != 0
}

/// Start this program again with `args`, detached from our stdio.
pub fn spawn_self(args: &[&str]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap it when it exits so no zombie is left behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn autostart_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("autostart").join(AUTOSTART_FILE))
}

pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|p| p.exists())
}

pub fn set_autostart(enabled: bool, simulate: bool) -> io::Result<()> {
    let path = autostart_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    if !enabled {
        return match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy();
    let extra = if simulate { " --simulate" } else { "" };
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Synapse for Linux (tray)\n\
         Comment=Battery status and quick settings for Razer devices\n\
         Exec=\"{}\" --tray{extra}\n\
         Icon=synapse-linux\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n",
        exe.replace('\\', "\\\\").replace('"', "\\\"")
    );
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, entry)
}
