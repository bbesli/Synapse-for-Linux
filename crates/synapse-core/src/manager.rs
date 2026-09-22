//! Background device manager shared by the GUI and the tray.
//!
//! A worker thread owns the device: it discovers and (re)connects it,
//! polls the values that change on their own, listens for changes made by
//! other programs or by the headset buttons, and executes requests from the
//! UI. The UI only ever reads [`Snapshot`]s, so it never blocks on USB I/O.

use std::collections::{BTreeSet, VecDeque};
use std::mem;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::Error;
use crate::devices::blackshark_v2_hs::protocol::{self, DOMAIN_LOCAL, reg};
use crate::devices::blackshark_v2_hs::{HeadsetState, Incoming, LedMode};
use crate::devices::{self, BlackSharkV2Hs, Connection, Device, FoundDevice};
use crate::eq::{Bands, HardwarePreset};
use crate::hid::Transport;

/// Telephony report of the dongle (hook switch / phone mute).
const TELEPHONY_REPORT_ID: u8 = 0x05;
const LOG_LINES: usize = 200;

#[derive(Debug, Clone)]
pub struct ManagerOptions {
    /// How often battery, charging, mute and the active preset are polled.
    pub poll_interval: Duration,
    /// How often to look for a device while none is connected.
    pub rescan_interval: Duration,
    /// Use the simulated headset instead of real hardware.
    pub simulate: bool,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            rescan_interval: Duration::from_secs(2),
            simulate: devices::simulation_requested(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "phase", content = "detail")]
pub enum Phase {
    /// No supported device plugged in.
    NoDevice,
    /// A device exists but its hidraw node is not accessible (udev rule missing).
    PermissionDenied(String),
    /// Opening the device / reading its state.
    Connecting,
    /// Dongle present, headset off or out of range.
    HeadsetOffline,
    /// Headset reachable.
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceSummary {
    pub name: &'static str,
    pub vid: u16,
    pub pid: u16,
    pub connection: Connection,
    pub path: String,
    pub simulated: bool,
    pub experimental: bool,
}

impl DeviceSummary {
    fn from_found(found: &FoundDevice) -> Self {
        Self {
            name: found.model.name,
            vid: found.model.vid,
            pid: found.model.pid,
            connection: found.model.connection,
            path: found.path_display(),
            simulated: found.is_simulated(),
            experimental: found.model.experimental,
        }
    }
}

/// Why the last action failed, for localized display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NotConnected,
    HeadsetOffline,
    Timeout,
    Busy,
    Other(String),
}

impl Problem {
    fn from_error(err: &Error) -> Self {
        match err {
            Error::NoDevice | Error::Disconnected | Error::PermissionDenied { .. } => Problem::NotConnected,
            Error::HeadsetOffline => Problem::HeadsetOffline,
            Error::Timeout { .. } | Error::EmptyResponse { .. } => Problem::Timeout,
            Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => Problem::Busy,
            other => Problem::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub time: SystemTime,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub phase: Phase,
    pub device: Option<DeviceSummary>,
    pub state: HeadsetState,
    /// Requests queued or running.
    pub pending: usize,
    /// Last failed action; the counter changes with every new failure.
    pub problem: Option<(u64, Problem)>,
    /// Changes with every update.
    pub revision: u64,
    pub log: VecDeque<LogLine>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            phase: Phase::NoDevice,
            device: None,
            state: HeadsetState::default(),
            pending: 0,
            problem: None,
            revision: 0,
            log: VecDeque::new(),
        }
    }
}

impl Snapshot {
    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }
}

/// Actions the UI can ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Refresh,
    SetSidetone(bool),
    SetSidetoneLevel(u8),
    SetSleepMinutes(u8),
    SetBtDnd(bool),
    SetLedMode(LedMode),
    SelectPreset(HardwarePreset),
    ApplyCustomEq(Bands),
    SetEnhancement(bool),
    SetEqEnabled(bool),
}

enum Msg {
    Request(Request),
    Shutdown,
}

type Notify = Arc<dyn Fn() + Send + Sync>;

pub struct DeviceManager {
    tx: Sender<Msg>,
    shared: Arc<Mutex<Snapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl DeviceManager {
    /// Start the worker. `notify` runs (on the worker thread) after every change.
    pub fn start(options: ManagerOptions, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let worker = Worker {
            options,
            shared: Arc::clone(&shared),
            notify: Arc::new(notify),
            rx,
            device: None,
            next_scan: Instant::now(),
            next_poll: Instant::now(),
            dirty: BTreeSet::new(),
        };
        let handle = thread::Builder::new()
            .name("synapse-device".into())
            .spawn(move || worker.run())
            .expect("spawn device thread");
        Self {
            tx,
            shared,
            worker: Some(handle),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared).clone()
    }

    pub fn request(&self, request: Request) {
        lock(&self.shared).pending += 1;
        if self.tx.send(Msg::Request(request)).is_err() {
            lock(&self.shared).pending -= 1;
        }
    }
}

impl Drop for DeviceManager {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

fn lock(shared: &Mutex<Snapshot>) -> MutexGuard<'_, Snapshot> {
    // A panic while holding the lock leaves plain data behind; keep going.
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

type Headset = BlackSharkV2Hs<Box<dyn Transport>>;

struct Worker {
    options: ManagerOptions,
    shared: Arc<Mutex<Snapshot>>,
    notify: Notify,
    rx: Receiver<Msg>,
    device: Option<Headset>,
    next_scan: Instant,
    next_poll: Instant,
    /// GET registers to re-read because another program changed them.
    dirty: BTreeSet<u8>,
}

impl Worker {
    fn run(mut self) {
        loop {
            let wait = if self.device.is_some() {
                Duration::from_millis(100)
            } else {
                self.next_scan
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(500))
            };
            match self.rx.recv_timeout(wait) {
                Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Msg::Request(first)) => {
                    if !self.run_requests(first) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
    }

    /// Execute `first` plus everything queued behind it, keeping only the
    /// newest request of each kind (e.g. slider drags), in the order of the
    /// newest ones. Returns false on shutdown.
    fn run_requests(&mut self, first: Request) -> bool {
        let mut batch = vec![first];
        let mut shutdown = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Request(req) => batch.push(req),
                Msg::Shutdown => shutdown = true,
            }
        }
        let total = batch.len();
        let mut coalesced: Vec<Request> = Vec::new();
        for req in batch {
            // Drop the older request of the same kind; the newer one goes last
            // so [Apply(a), Select(b), Apply(c)] ends with Apply(c).
            coalesced.retain(|r| mem::discriminant(r) != mem::discriminant(&req));
            coalesced.push(req);
        }
        let skipped = total - coalesced.len();
        if skipped > 0 {
            self.update(|s| s.pending = s.pending.saturating_sub(skipped));
        }
        for req in coalesced {
            if !shutdown {
                self.execute(req);
            }
            self.update(|s| s.pending = s.pending.saturating_sub(1));
        }
        !shutdown
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.device.is_none() {
            if now >= self.next_scan {
                self.next_scan = now + self.options.rescan_interval;
                self.connect();
            }
            return;
        }
        self.listen();
        if !self.dirty.is_empty() {
            self.reread_dirty();
        }
        if Instant::now() >= self.next_poll {
            self.next_poll = Instant::now() + self.options.poll_interval;
            self.poll();
        }
    }

    // ----- connection ---------------------------------------------------

    fn connect(&mut self) {
        let found = if self.options.simulate {
            devices::simulated()
        } else {
            devices::discover()
        };
        let Some(found) = found.iter().find(|f| !f.model.experimental).or(found.first()).cloned() else {
            self.update(|s| {
                if s.phase != Phase::NoDevice || s.device.is_some() {
                    s.phase = Phase::NoDevice;
                    s.device = None;
                    s.state = HeadsetState::default();
                }
            });
            return;
        };

        let summary = DeviceSummary::from_found(&found);
        match devices::open(&found) {
            Ok(Device::BlackSharkV2Hs(dev)) => {
                self.log(format!("connected to {} ({})", summary.name, summary.path));
                self.update(|s| {
                    s.phase = Phase::Connecting;
                    s.device = Some(summary);
                });
                self.device = Some(dev);
                self.full_refresh();
                self.next_poll = Instant::now() + self.options.poll_interval;
            }
            Err(Error::PermissionDenied { path }) => {
                let path = path.display().to_string();
                self.update(|s| {
                    if s.phase != Phase::PermissionDenied(path.clone()) {
                        s.log_line(format!("no permission to open {path}"));
                    }
                    s.phase = Phase::PermissionDenied(path);
                    s.device = Some(summary);
                });
            }
            Err(err) => {
                self.log(format!("cannot open {}: {err}", summary.path));
                self.update(|s| {
                    s.phase = Phase::NoDevice;
                    s.device = Some(summary);
                });
            }
        }
    }

    fn disconnect(&mut self) {
        self.device = None;
        self.dirty.clear();
        self.next_scan = Instant::now();
        self.log("device disconnected".into());
        self.update(|s| {
            s.phase = Phase::NoDevice;
            s.device = None;
            s.state = HeadsetState::default();
        });
    }

    /// Handle an error from a background operation.
    fn background_error(&mut self, err: Error) {
        match err {
            Error::Disconnected => self.disconnect(),
            Error::HeadsetOffline => self.set_offline(),
            other => self.log(format!("device error: {other}")),
        }
    }

    fn set_offline(&mut self) {
        let was_ready = lock(&self.shared).phase == Phase::Ready;
        if was_ready {
            self.log("headset went offline".into());
        }
        self.update(|s| {
            s.phase = Phase::HeadsetOffline;
            s.state.clear_headset();
            s.state.link_up = Some(false);
        });
    }

    // ----- reading ------------------------------------------------------

    fn full_refresh(&mut self) {
        let Some(dev) = self.device.as_mut() else { return };
        match dev.read_state() {
            Ok(state) => {
                let online = state.link_up == Some(true);
                self.update(|s| {
                    s.state = state;
                    s.phase = if online { Phase::Ready } else { Phase::HeadsetOffline };
                });
            }
            Err(err) => {
                self.background_error(err);
                // Still connected but unreadable: show what we have.
                if self.device.is_some() && lock(&self.shared).phase == Phase::Connecting {
                    self.update(|s| s.phase = Phase::HeadsetOffline);
                }
            }
        }
    }

    fn poll(&mut self) {
        let Some(dev) = self.device.as_mut() else { return };
        let was_ready = lock(&self.shared).phase == Phase::Ready;
        match dev.link_up() {
            Ok(false) => {
                if was_ready || lock(&self.shared).state.link_up != Some(false) {
                    self.set_offline();
                }
            }
            Ok(true) if !was_ready => {
                self.log("headset connected".into());
                self.full_refresh();
            }
            Ok(true) => {
                let mut state = lock(&self.shared).state.clone();
                // The preset can also change from outside (headset button, other programs).
                let result = dev.refresh_status(&mut state).and_then(|()| {
                    state.eq_preset = Some(dev.eq_preset()?);
                    Ok(())
                });
                match result {
                    Ok(()) => self.update(|s| {
                        let preset = s.state.eq_preset;
                        s.state.battery = state.battery;
                        s.state.charging = state.charging;
                        s.state.mic_muted = state.mic_muted;
                        s.state.eq_preset = state.eq_preset.or(preset);
                    }),
                    Err(err) => self.background_error(err),
                }
            }
            Err(err) => self.background_error(err),
        }
    }

    /// Pick up replies and reports that were not for us.
    fn listen(&mut self) {
        let Some(dev) = self.device.as_mut() else { return };
        let connection = dev.connection();
        let events = match dev.poll_incoming(Duration::ZERO) {
            Ok(events) => events,
            Err(err) => return self.background_error(err),
        };
        if events.is_empty() {
            return;
        }
        let mut changed = false;
        {
            let mut snap = lock(&self.shared);
            let before = snap.state.clone();
            for event in events {
                match event {
                    // Another program changed a setting: read that register back.
                    // (Only SET echoes cause reads; replies to reads never do, so
                    // two programs watching the same device cannot ping-pong.)
                    Incoming::Frame(resp) if reg::is_set(resp.cmd) => {
                        self.dirty.insert(reg::get_of(resp.cmd));
                    }
                    // A reply to someone else's read: free information.
                    Incoming::Frame(resp) => {
                        snap.state.apply_reply(&resp, connection);
                    }
                    Incoming::Other {
                        report_id: TELEPHONY_REPORT_ID,
                        ..
                    } => {
                        // Mute button on the headset.
                        self.dirty.insert(reg::MIC_MUTE);
                    }
                    Incoming::Other { .. } => {}
                }
            }
            if snap.state != before {
                changed = true;
                snap.revision += 1;
            }
        }
        if changed {
            (self.notify)();
        }
    }

    fn reread_dirty(&mut self) {
        let Some(dev) = self.device.as_mut() else { return };
        let registers = mem::take(&mut self.dirty);
        let connection = dev.connection();
        let headset_domain = dev.headset_domain();
        let mut replies = Vec::new();
        for get in registers {
            let domain = if matches!(get, reg::DONGLE_LED | reg::LINK_STATUS) {
                DOMAIN_LOCAL
            } else {
                headset_domain
            };
            match dev.transact(domain, get, &[]) {
                Ok(resp) => replies.push(resp),
                Err(err) if err.is_timeout() => {}
                Err(err) => return self.background_error(err),
            }
        }
        self.update(|s| {
            for resp in &replies {
                s.state.apply_reply(resp, connection);
            }
        });
    }

    // ----- actions ------------------------------------------------------

    fn execute(&mut self, req: Request) {
        if matches!(req, Request::Refresh) {
            if self.device.is_none() {
                self.next_scan = Instant::now();
                self.connect();
            } else {
                self.full_refresh();
            }
            return;
        }
        let Some(dev) = self.device.as_mut() else {
            return self.fail(&req, Error::NoDevice);
        };
        let offline = lock(&self.shared).phase == Phase::HeadsetOffline;
        let needs_headset = !matches!(req, Request::SetLedMode(_));
        if offline && needs_headset {
            return self.fail(&req, Error::HeadsetOffline);
        }

        let mut state = lock(&self.shared).state.clone();
        let result = apply_request(dev, &mut state, &req);
        match result {
            Ok(()) => {
                log::debug!("done: {req:?}");
                self.update(|s| s.state = state);
            }
            Err(err) => self.fail(&req, err),
        }
    }

    fn fail(&mut self, req: &Request, err: Error) {
        self.log(format!("{req:?} failed: {err}"));
        let problem = Problem::from_error(&err);
        self.update(|s| {
            let id = s.problem.as_ref().map_or(1, |(id, _)| id + 1);
            s.problem = Some((id, problem));
        });
        match err {
            Error::Disconnected => self.disconnect(),
            Error::HeadsetOffline => self.set_offline(),
            _ => {}
        }
    }

    // ----- plumbing -----------------------------------------------------

    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        {
            let mut snap = lock(&self.shared);
            f(&mut snap);
            snap.revision += 1;
        }
        (self.notify)();
    }

    fn log(&self, text: String) {
        log::info!("{text}");
        self.update(|s| s.log_line(text));
    }
}

impl Snapshot {
    fn log_line(&mut self, text: String) {
        if self.log.len() >= LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(LogLine {
            time: SystemTime::now(),
            text,
        });
    }
}

/// Perform one request and update `state` with what the device reports back.
fn apply_request(dev: &mut Headset, state: &mut HeadsetState, req: &Request) -> crate::Result<()> {
    match *req {
        Request::Refresh => Ok(()),
        Request::SetSidetone(on) => {
            dev.set_sidetone_enabled(on)?;
            if on {
                // Re-send the level: the DSP may come back from "off" at a default.
                if let Some(level) = state.sidetone_level {
                    dev.set_sidetone_level(level)?;
                }
            }
            state.sidetone_enabled = Some(dev.sidetone_enabled()?);
            Ok(())
        }
        Request::SetSidetoneLevel(level) => {
            dev.set_sidetone_level(level.min(protocol::SIDETONE_MAX))?;
            state.sidetone_level = Some(dev.sidetone_level()?);
            Ok(())
        }
        Request::SetSleepMinutes(minutes) => {
            dev.set_sleep_minutes(minutes)?;
            state.sleep_minutes = Some(dev.sleep_minutes()?);
            Ok(())
        }
        Request::SetBtDnd(on) => {
            dev.set_bt_dnd(on)?;
            state.bt_dnd = Some(dev.bt_dnd()?);
            Ok(())
        }
        Request::SetLedMode(mode) => {
            dev.set_led_mode(mode)?;
            state.led_mode = dev.led_mode()?;
            Ok(())
        }
        Request::SelectPreset(preset) => {
            dev.select_preset(preset)?;
            state.eq_enabled = Some(true);
            dev.refresh_eq(state)
        }
        Request::ApplyCustomEq(bands) => {
            dev.apply_custom_eq(&bands)?;
            state.eq_enabled = Some(true);
            dev.refresh_eq(state)
        }
        Request::SetEnhancement(on) => {
            dev.set_enhancement(on)?;
            state.enhancement = Some(dev.enhancement()?);
            Ok(())
        }
        Request::SetEqEnabled(on) => {
            dev.set_eq_enabled(on)?;
            state.eq_enabled = Some(dev.eq_enabled()?);
            Ok(())
        }
    }
}

/// `HH:MM:SS` in local time.
pub fn format_clock(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    // SAFETY: localtime_r only writes into `tm`.
    let mut tm: libc::tm = unsafe { mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok {
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        String::from("--:--:--")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::blackshark_v2_hs::sim::SimTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn wait_for(manager: &DeviceManager, what: &str, cond: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snap = manager.snapshot();
            if cond(&snap) {
                return snap;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {snap:#?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// End-to-end over the simulator: connect, read, change settings.
    #[test]
    fn manager_drives_simulated_headset() {
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let manager = DeviceManager::start(
            ManagerOptions {
                poll_interval: Duration::from_millis(200),
                rescan_interval: Duration::from_millis(50),
                simulate: true,
            },
            move || {
                counter.fetch_add(1, Ordering::Relaxed);
            },
        );

        let snap = wait_for(&manager, "ready", Snapshot::is_ready);
        assert_eq!(snap.state.battery, Some(76));
        assert!(snap.device.as_ref().unwrap().simulated);
        assert!(notified.load(Ordering::Relaxed) > 0);

        manager.request(Request::SetSidetone(true));
        manager.request(Request::SetSidetoneLevel(3));
        manager.request(Request::SetSidetoneLevel(9)); // coalesced with the previous one
        manager.request(Request::SelectPreset(HardwarePreset::Game));
        let snap = wait_for(&manager, "settings applied", |s| {
            s.pending == 0 && s.state.sidetone_level == Some(9)
        });
        assert_eq!(snap.state.sidetone_enabled, Some(true));
        assert_eq!(snap.state.eq_preset, Some(HardwarePreset::Game));
        assert_eq!(snap.state.custom_eq, Some([2, 1, 0, 0, 0, 0, 0, 1, 2, 1]));

        let curve = [1, 2, 3, 4, 5, 6, -1, -2, -3, -9];
        manager.request(Request::ApplyCustomEq(curve));
        let snap = wait_for(&manager, "custom eq", |s| s.state.custom_eq == Some(curve));
        assert_eq!(snap.state.eq_preset, Some(HardwarePreset::Custom));
        assert!(snap.problem.is_none(), "{:?}", snap.problem);

        manager.request(Request::SetSidetoneLevel(15));
        manager.request(Request::SetLedMode(LedMode::Off));
        manager.request(Request::SetSleepMinutes(0));
        let snap = wait_for(&manager, "misc applied", |s| {
            s.pending == 0 && s.state.led_mode == Some(LedMode::Off)
        });
        assert_eq!(snap.state.sleep_minutes, Some(0));
        assert_eq!(snap.state.sidetone_level, Some(15));
    }

    /// Lets a test inject frames into a simulated device the worker owns.
    struct SharedSim(Arc<Mutex<SimTransport>>);

    impl Transport for SharedSim {
        fn write_report(&mut self, report: &[u8]) -> crate::Result<()> {
            self.0.lock().unwrap().write_report(report)
        }
        fn read_report(&mut self, buf: &mut [u8], timeout: Duration) -> crate::Result<Option<usize>> {
            let got = self.0.lock().unwrap().read_report(buf, Duration::ZERO)?;
            if got.is_none() && !timeout.is_zero() {
                thread::sleep(timeout.min(Duration::from_millis(5)));
            }
            Ok(got)
        }
    }

    fn test_worker() -> (Worker, Arc<Mutex<SimTransport>>, Sender<Msg>) {
        let sim = Arc::new(Mutex::new(SimTransport::new(Connection::Dongle)));
        let transport: Box<dyn Transport> = Box::new(SharedSim(Arc::clone(&sim)));
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            options: ManagerOptions::default(),
            shared: Arc::new(Mutex::new(Snapshot {
                phase: Phase::Ready,
                ..Snapshot::default()
            })),
            notify: Arc::new(|| {}),
            rx,
            device: Some(BlackSharkV2Hs::new(transport, &devices::MODELS[0])),
            next_scan: Instant::now(),
            next_poll: Instant::now() + Duration::from_secs(3600),
            dirty: BTreeSet::new(),
        };
        (worker, sim, tx)
    }

    /// Replies to *other* programs' reads must never trigger reads of our own,
    /// otherwise the GUI and the tray keep answering each other forever.
    #[test]
    fn foreign_replies_update_state_without_triggering_reads() {
        let (mut worker, sim, _tx) = test_worker();
        let frame = |cmd, payload: &[u8]| protocol::encode_response(0x61, 0x80, cmd, 1, payload).to_vec();

        sim.lock().unwrap().inject(frame(reg::EQ_PRESET, &[0x07]));
        sim.lock().unwrap().inject(frame(reg::BATTERY, &[42]));
        worker.listen();
        assert!(worker.dirty.is_empty(), "{:?}", worker.dirty);
        let state = lock(&worker.shared).state.clone();
        assert_eq!(state.eq_preset, Some(HardwarePreset::Game));
        assert_eq!(state.battery, Some(42));

        // A SET echo means another program changed something: re-read just that.
        sim.lock().unwrap().inject(frame(reg::set(reg::SIDETONE_LEVEL), &[0]));
        worker.listen();
        assert_eq!(worker.dirty, BTreeSet::from([reg::SIDETONE_LEVEL]));
        let log = sim.lock().unwrap().request_log();
        let before = log.lock().unwrap().len();
        worker.reread_dirty();
        let requests = log.lock().unwrap()[before..].to_vec();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].cmd, reg::SIDETONE_LEVEL);
        assert!(worker.dirty.is_empty());
    }

    /// When requests pile up, the user's last action must win.
    #[test]
    fn coalesced_requests_keep_the_order_of_the_newest() {
        let (mut worker, _sim, tx) = test_worker();
        let (b1, b2) = ([1; 10], [2; 10]);
        tx.send(Msg::Request(Request::SelectPreset(HardwarePreset::Game)))
            .unwrap();
        tx.send(Msg::Request(Request::ApplyCustomEq(b2))).unwrap();
        lock(&worker.shared).pending = 3;
        assert!(worker.run_requests(Request::ApplyCustomEq(b1)));
        let snap = lock(&worker.shared).clone();
        assert_eq!(snap.state.eq_preset, Some(HardwarePreset::Custom));
        assert_eq!(snap.state.custom_eq, Some(b2));
        assert_eq!(snap.pending, 0);
    }

    #[test]
    fn clock_format() {
        let text = format_clock(SystemTime::now());
        assert_eq!(text.len(), 8);
        assert_eq!(&text[2..3], ":");
    }
}
