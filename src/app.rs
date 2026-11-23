//! DDE3 Toolkit GUI — egui desktop application for BMW EDC15 DDE3.0 over K-line.
//!
//! Mirrors dde4-toolkit's app structure: Overview, Read flash, Write flash,
//! Checksums, Fault codes, Live data, Value logger, Protocol log tabs.
//!
//! ⚠️ The live-data response layout is UNRESOLVED for DDE3 (see HANDOVER.md §14).
//! Fault-code read/clear use the DS2 FS_* frames extracted from DDE30DS0.prg
//! (`12 05 14 01` count, `12 05 04 00` entry, `12 05 04 01` shadow,
//! `12 04 05` clear) with the German text tables in `crate::dtc`.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, ScrollArea, Stroke,
    TextStyle, Ui, Vec2, pos2, vec2,
};

use crate::checksum::{self, CAL_LEN, CAL_START, IMAGE_SIZE};
use crate::live::{LiveVar, LIVE_VARS};
use crate::patches::{self, Patch, PatchState};
use crate::protocol::{self, Kline};

const BG: Color32 = Color32::from_rgb(10, 14, 18);
const PANEL: Color32 = Color32::from_rgb(14, 19, 24);
const CARD: Color32 = Color32::from_rgb(20, 27, 34);
const CARD_ALT: Color32 = Color32::from_rgb(25, 33, 41);
const BORDER: Color32 = Color32::from_rgb(38, 49, 59);
const TEXT: Color32 = Color32::from_rgb(226, 235, 242);
const MUTED: Color32 = Color32::from_rgb(137, 153, 166);
const ACCENT: Color32 = Color32::from_rgb(75, 211, 194);
const ACCENT_DIM: Color32 = Color32::from_rgb(38, 105, 100);
const WARN: Color32 = Color32::from_rgb(240, 180, 41);
const DANGER: Color32 = Color32::from_rgb(239, 96, 96);
const OK: Color32 = Color32::from_rgb(94, 214, 141);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Full,
    Cal,
    Write,
    WriteCal,
    Dtc,
    DtcClear,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Read,
    Write,
    Checksum,
    Patches,
    Dtc,
    Live,
    Logger,
    Log,
}

impl Tab {
    pub fn parse(s: &str) -> Self {
        match s {
            "dtc" | "errors" | "faults" => Tab::Dtc,
            "read" => Tab::Read,
            "write" => Tab::Write,
            "checksum" => Tab::Checksum,
            "patches" | "patch" => Tab::Patches,
            "live" => Tab::Live,
            "logger" => Tab::Logger,
            "log" => Tab::Log,
            _ => Tab::Overview,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Read => "Read flash",
            Tab::Write => "Write flash",
            Tab::Checksum => "Checksums",
            Tab::Patches => "Patches",
            Tab::Dtc => "Fault codes",
            Tab::Live => "Live data",
            Tab::Logger => "Value logger",
            Tab::Log => "Protocol log",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Tab::Overview => "◈",
            Tab::Read => "▤",
            Tab::Write => "▥",
            Tab::Checksum => "Σ",
            Tab::Patches => "⊞",
            Tab::Dtc => "⚡",
            Tab::Live => "◉",
            Tab::Logger => "▦",
            Tab::Log => "≡",
        }
    }
}

#[derive(Clone)]
struct EcuInfo {
    /// Parsed IDENT string (software version, ECU code, date, Bosch HW
    /// number, G code). `None` when the ident looked erased/corrupt.
    ident: Option<protocol::IdentInfo>,
    hardware: String,
}

// ---------------------------------------------------------------------------
// CSV value logger
// ---------------------------------------------------------------------------

struct LogWriter {
    file: BufWriter<File>,
    path: PathBuf,
    rows: usize,
    start: Instant,
    ids: Vec<u16>,
}

/// Default log file name, matching the WinOLS/testo log naming
/// `YYYY-MM-DD-HHMMSS.csv` (e.g. `2026-08-12-160605.csv`, see
/// tuning/testing/logs/).
fn default_log_file_name() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let time = secs;
    // Use thread-safe time functions with platform-specific variants.
    let tm = unsafe {
        #[cfg(unix)]
        {
            let mut t = std::mem::MaybeUninit::<libc::tm>::uninit();
            if libc::localtime_r(&time, t.as_mut_ptr()).is_null() {
                let mut u = std::mem::MaybeUninit::<libc::tm>::uninit();
                if libc::gmtime_r(&time, u.as_mut_ptr()).is_null() {
                    return "log.csv".into();
                }
                *u.as_ptr()
            } else {
                *t.as_ptr()
            }
        }
        #[cfg(windows)]
        {
            let mut t = std::mem::MaybeUninit::<libc::tm>::uninit();
            if libc::localtime_s(t.as_mut_ptr(), &time) != 0 {
                let mut u = std::mem::MaybeUninit::<libc::tm>::uninit();
                if libc::gmtime_s(u.as_mut_ptr(), &time) != 0 {
                    return "log.csv".into();
                }
                *u.as_ptr()
            } else {
                *t.as_ptr()
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            return "log.csv".into();
        }
    };
    format!(
        "{:04}-{:02}-{:02}-{:02}{:02}{:02}.csv",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

impl LogWriter {
    fn write_row(&mut self, pairs: &[(u16, u16)]) {
        let t = self.start.elapsed().as_secs_f32();
        let _ = write!(self.file, "{t:.3}");
        for id in &self.ids {
            let raw = pairs
                .iter()
                .find(|(pid, _)| pid == id)
                .map(|(_, v)| *v)
                .unwrap_or(0);
            let i = LIVE_VARS.iter().position(|v| v.id == *id);
            let phys = match i {
                Some(i) => raw as f64 * LIVE_VARS[i].factor + LIVE_VARS[i].offset,
                None => raw as f64,
            };
            let _ = write!(self.file, ",{phys:.4}");
        }
        let _ = writeln!(self.file);
        self.rows += 1;
        if self.rows.is_multiple_of(20) {
            let _ = self.file.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Messages between worker threads and the GUI
// ---------------------------------------------------------------------------

enum Msg {
    Log(String),
    Status(String),
    Connected(EcuInfo),
    Error(String),
    LinkLost(String),
    Progress(f32, String),
    ReadDone(Vec<u8>, PathBuf),
    CalDone(PathBuf, usize),
    WriteDone,
    DtcDone(Vec<protocol::Dtc>),
    DtcCleared,
    Battery(f64),
    /// Pre-job battery snapshot (taken inside a running read/write job). Only
    /// refreshes the displayed voltage — unlike [`Msg::Battery`] it must NOT
    /// clear `busy` / `active_job`, or the running job's progress bar
    /// disappears the moment the snapshot arrives.
    BatterySnapshot(f64),
    /// Live values + the producer-measured polling rate (Hz, 1 s sliding
    /// window in the live thread; 0.0 only on the first sample of a session).
    Live(Vec<(u16, u16)>, f32),
    LogProgress(usize),
    /// Fast-baud (0x91) switch succeeded; payload = the rate the port now runs at.
    FastBaud(u32),
    /// Fast-baud switch failed; payload = reason.
    FastBaudFailed(String),
}

pub struct App {
    link: Arc<Mutex<Option<Kline>>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: bool,
    connected: bool,
    ports: Vec<String>,
    port: String,
    tab: Tab,
    status: String,
    error: Option<String>,
    ecu: Option<EcuInfo>,
    logs: Vec<String>,
    /// Full run log, one file per launch in the temp dir — never truncated,
    /// unlike the in-app panel (which keeps the last 2000 lines).
    run_log: Option<BufWriter<File>>,
    run_log_path: PathBuf,
    read_path: String,
    cal_path: String,
    cal_info: Option<(String, usize)>,
    dtcs: Vec<protocol::Dtc>,
    dtc_clear_armed: bool,
    battery_v: Option<f64>,
    post_write_pending: bool,
    image: Option<Vec<u8>>,
    image_name: String,
    image_is_partial: bool,
    checks: Vec<checksum::Checksum>,
    comp_edit: Vec<String>,
    progress: f32,
    progress_text: String,
    active_job: Option<Job>,
    /// When the currently displayed job started — the elapsed-time timer
    /// overlaid on the progress bar. Set by the `start_*` functions; only
    /// read while `active_job.is_some()`, so a stale value after the job
    /// ends is never drawn.
    job_started: Option<Instant>,
    live_running: bool,
    live_stop: Arc<AtomicBool>,
    live_values: Vec<f64>,
    live_history: Vec<Vec<f32>>,
    live_enabled: Vec<bool>,
    live_rate: f32,
    live_log: Arc<Mutex<Option<LogWriter>>>,
    logging: bool,
    log_path: String,
    log_rows: usize,
    show_graph: bool,
    show_var_picker: bool,
    graph_var: usize,
    backup_ok: bool,
    write_confirm: String,
    auto_live: bool,
    auto_read: bool,
    auto_cal: bool,
    auto_log: Option<PathBuf>,
    auto_write_cal: bool,
    auto_write: bool,
    /// Requested fast-baud target from the CLI (`--fast-baud`), or None.
    fast_baud: Option<u32>,
    /// The rate the DS2 link currently runs at once a 0x91 switch succeeded.
    fast_baud_active: Option<u32>,
    /// "faster 9600" test switch (Live tab): back-to-back polling (1 ms gap),
    /// 1 ms/byte TX pacing. Off by default — the link runs the original
    /// WinKFP timing (55 ms poll gap, 3 ms/byte).
    ///
    /// Shared with the live thread; can be flipped while live is running.
    fast9600: Arc<AtomicBool>,
    /// Firmware patch manifests (built in + `patches/` + DDE3_PATCH_DIR).
    patches: Vec<Patch>,
    patch_selected: usize,
    patch_warnings: Vec<String>,
    patch_notice: Option<String>,
    patch_target_ok: Option<bool>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        demo: bool,
        initial_tab: Tab,
        autoconnect: bool,
        auto_live: bool,
        auto_read: bool,
        auto_cal: bool,
        auto_log: Option<PathBuf>,
        show_vars: bool,
        port: Option<String>,
        image: Option<PathBuf>,
        auto_write_cal: bool,
        auto_write: bool,
        fast_baud: Option<u32>,
        log_dir: Option<PathBuf>,
    ) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = unbounded();
        let (patches, patch_warnings) = patches::load_all();
        let mut app = Self {
            link: Arc::new(Mutex::new(None)),
            tx,
            rx,
            busy: false,
            connected: false,
            ports: Vec::new(),
            port: String::new(),
            tab: initial_tab,
            status: "Ready".into(),
            error: None,
            ecu: None,
            logs: vec!["DDE3 Toolkit started".into()],
            run_log: None,
            run_log_path: PathBuf::new(),
            read_path: default_path("dde3_full.bin"),
            cal_path: default_path("dde3_cal.bin"),
            cal_info: None,
            dtcs: Vec::new(),
            dtc_clear_armed: false,
            battery_v: None,
            post_write_pending: false,
            image: None,
            image_name: String::new(),
            image_is_partial: false,
            checks: Vec::new(),
            comp_edit: Vec::new(),
            progress: 0.0,
            progress_text: String::new(),
            active_job: None,
            job_started: None,
            live_running: false,
            live_stop: Arc::new(AtomicBool::new(false)),
            live_values: vec![0.0; LIVE_VARS.len()],
            live_history: vec![Vec::new(); LIVE_VARS.len()],
            live_enabled: LIVE_VARS.iter().map(|v| v.default_on).collect(),
            live_rate: 0.0,
            live_log: Arc::new(Mutex::new(None)),
            logging: false,
            log_path: String::new(),
            log_rows: 0,
            show_graph: true,
            show_var_picker: show_vars,
            graph_var: 0,
            backup_ok: false,
            write_confirm: String::new(),
            auto_live,
            auto_read,
            auto_cal,
            auto_log,
            auto_write_cal,
            auto_write,
            fast_baud,
            fast_baud_active: None,
            fast9600: Arc::new(AtomicBool::new(false)),
            patches,
            patch_selected: 0,
            patch_warnings,
            patch_notice: None,
            patch_target_ok: None,
        };
        app.open_run_log(demo, log_dir);
        app.refresh_ports();
        if demo {
            app.demo();
        }
        if let Some(path) = image {
            app.load_image(path);
        }
        if autoconnect {
            if let Some(p) = port {
                if !p.is_empty() {
                    app.port = p;
                }
            }
            app.connect();
        }
        app
    }

    /// Fake connected state for UI development without hardware.
    fn demo(&mut self) {
        self.connected = true;
        self.status = "Demo mode".into();
        // Values from the 2026-11-10 real-car capture so the UI renders as
        // it does with hardware.
        self.ecu = Some(EcuInfo {
            ident: Some(protocol::IdentInfo {
                software: "7786887".into(),
                ecu_code: "5201".into(),
                date: "4801".into(),
                hw_number: "0281001445".into(),
                g_code: "G521".into(),
            }),
            hardware: "DDE3.0 SIM".into(),
        });
        self.port = "/dev/ttyUSB1".into();
        let mut img = vec![0u8; IMAGE_SIZE];
        img = checksum::correct(&img);
        self.checks = checksum::verify(&img);
        self.image = Some(img);
        self.image_name = "demo_dde3.bin".into();
        self.image_is_partial = false;
        self.dtcs = vec![
            protocol::Dtc {
                ort: 0x26,
                art: 0x02,
                uw: vec![437, 659],
                ..Default::default()
            },
            protocol::Dtc {
                ort: 0x05,
                art: 0x01,
                uw: vec![1371, 1593],
                ..Default::default()
            },
            protocol::Dtc {
                ort: 0x36,
                art: 0x04,
                uw: vec![837, 1059],
                ..Default::default()
            },
        ];
        self.battery_v = Some(13.9);
        self.sync_comp_edits();
        // Demo log lines follow the verified real-car wire convention
        // (docs/wire-protocol.md): the ECU replies with its own address
        // 0x12, status byte at [2]; IDENT answers with a bare ASCII string.
        self.logs = vec![
            "DDE3 Toolkit started (demo mode)".into(),
            "TX 12 04 00 16".into(),
            "RX 44 44 45 33 2E 30 20 46 55 45 52 20 4D 34 37".into(),
            "TX 12 05 14 01 31".into(),
            "RX 12 05 A0 03".into(),
        ];
    }

    fn refresh_ports(&mut self) {
        self.ports = serialport::available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.port_name)
            .collect();
        if self.port.is_empty() {
            self.port = self
                .ports
                .iter()
                .find(|p| p.contains("USB"))
                .cloned()
                .or_else(|| self.ports.first().cloned())
                .unwrap_or_default();
        }
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Log(line) => self.push_log(line),
                Msg::Status(s) => self.status = s,
                Msg::Connected(info) => {
                    self.connected = true;
                    self.busy = false;
                    self.ecu = Some(info);
                    self.status = "Connected".into();
                    // With a CLI --fast-baud target, switch first; the auto
                    // actions run once Msg::FastBaud confirms the new rate.
                    // Otherwise run them now. (The Live-tab button is a one-off
                    // and does not set self.fast_baud, so it never re-fires here.)
                    if let Some(rate) = self.fast_baud {
                        self.switch_fast_baud_async(rate);
                    } else {
                        self.start_auto_actions();
                    }
                }
                Msg::FastBaud(rate) => {
                    self.busy = false;
                    self.fast_baud_active = Some(rate);
                    self.status = format!("DS2 link at {rate} baud");
                    self.push_log(format!("FASTBAUD {rate} (revert by power cycle)"));
                    // Run any pending automation now that the link is fast.
                    self.start_auto_actions();
                }
                Msg::FastBaudFailed(e) => {
                    self.busy = false;
                    self.fast_baud_active = None;
                    self.error = Some(format!("fast-baud switch failed: {e}"));
                    self.push_log(format!("FASTBAUD FAILED {e}"));
                    // The link is back at 9600 (switch_fast_baud falls back on
                    // failure). Still honor any requested automation, at 9600, so
                    // --fast-baud --auto-live is not a silent no-op.
                    self.start_auto_actions();
                }
                Msg::Error(e) => {
                    self.busy = false;
                    self.active_job = None;
                    self.progress_text = "Aborted".into();
                    self.error = Some(e.clone());
                    self.status = "Error".into();
                    self.push_log(format!("ERROR {e}"));
                }
                Msg::LinkLost(e) => {
                    self.busy = false;
                    self.active_job = None;
                    self.progress_text = "Link lost".into();
                    self.stop_live();
                    self.fast_baud_active = None;
                    *lock_or_recover(&self.link) = None;
                    self.connected = false;
                    self.error = Some(format!("K-line link lost: {e}. Check ECU power and the USB cable, then press Connect."));
                    self.status = "Link lost".into();
                    self.push_log(format!("LINK LOST {e}"));
                }
                Msg::Progress(p, text) => {
                    self.progress = p;
                    self.progress_text = text;
                }
                Msg::CalDone(path, size) => {
                    self.busy = false;
                    self.active_job = None;
                    self.progress = 1.0;
                    self.progress_text = format!("Saved {} ({} bytes)", path.display(), size);
                    self.cal_info = Some((path.display().to_string(), size));
                    self.status = "Calibration read complete".into();
                }
                Msg::ReadDone(data, path) => {
                    self.busy = false;
                    self.active_job = None;
                    self.progress = 1.0;
                    self.progress_text = format!("Saved {}", path.display());
                    self.checks = checksum::verify(&data);
                    self.image = Some(data);
                    self.image_name = path.display().to_string();
                    self.status = "Read complete".into();
                }
                Msg::WriteDone => {
                    self.busy = false;
                    self.active_job = None;
                    self.progress = 1.0;
                    self.progress_text =
                        "Write and verify complete; power cycle the ECU before clearing faults"
                            .into();
                    self.status = "Write complete — power cycle the ECU".into();
                    self.post_write_pending = true;
                }
                Msg::DtcDone(dtcs) => {
                    self.busy = false;
                    self.active_job = None;
                    self.dtc_clear_armed = false;
                    self.status = if dtcs.is_empty() {
                        "No stored fault codes".into()
                    } else {
                        format!("{} fault(s) stored", dtcs.len())
                    };
                    self.dtcs = dtcs;
                }
                Msg::DtcCleared => {
                    self.busy = false;
                    self.active_job = None;
                    self.dtcs.clear();
                    self.dtc_clear_armed = false;
                    self.status = "Fault memory cleared".into();
                }
                Msg::Battery(v) => {
                    self.battery_v = Some(v);
                    self.busy = false;
                    self.active_job = None;
                    self.status = format!("Battery {v:.2} V");
                }
                Msg::BatterySnapshot(v) => {
                    // Snapshot from inside a running job: update the voltage
                    // display only. The job's own Done/Error message clears
                    // `busy` / `active_job` and sets the final status.
                    self.battery_v = Some(v);
                }
                Msg::LogProgress(rows) => {
                    self.log_rows = rows;
                }
                Msg::Live(vals, hz) => {
                    // Producer-measured cycle rate (1 s sliding window in the
                    // live thread). 0.0 only on the first sample of a session
                    // — keep the previous display value then.
                    if hz > 0.0 {
                        self.live_rate = hz;
                    }
                    for (id, raw) in vals {
                        if let Some(i) = LIVE_VARS.iter().position(|v| v.id == id) {
                            let v = raw as f64 * LIVE_VARS[i].factor + LIVE_VARS[i].offset;
                            self.live_values[i] = v;
                            let h = &mut self.live_history[i];
                            h.push(v as f32);
                            if h.len() > 240 {
                                h.remove(0);
                            }
                        }
                    }
                }
            }
        }
    }

    fn push_log(&mut self, line: String) {
        // The run-log file gets every line, untruncated (the panel below
        // keeps only the most recent 2000).
        if let Some(f) = self.run_log.as_mut() {
            let (_, stamp) = now_local();
            let _ = writeln!(f, "{stamp} {line}");
            let _ = f.flush();
        }
        self.logs.push(line);
        if self.logs.len() > 2000 {
            self.logs.drain(0..500);
        }
    }

    /// Open this run's full log file, named after the startup time:
    /// `<log dir>/dde3-YYYYmmdd-HHMMSS.log`. Every line pushed to the
    /// protocol log is appended here for the whole run. The log dir is the
    /// `--log-dir` flag, else `$DDE3_LOG_DIR`, else the persistent default
    /// (`$XDG_DATA_HOME/dde3-toolkit/logs` or `~/.local/share/...`).
    fn open_run_log(&mut self, demo: bool, log_dir: Option<PathBuf>) {
        let (name_stamp, human) = now_local();
        let dir = log_dir
            .or_else(|| {
                std::env::var("DDE3_LOG_DIR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(default_log_dir);
        let path = dir.join(format!("dde3-{name_stamp}.log"));
        let opened = std::fs::create_dir_all(&dir)
            .and_then(|_| OpenOptions::new().create(true).append(true).open(&path));
        match opened {
            Ok(file) => {
                let mut w = BufWriter::new(file);
                let _ = writeln!(w, "=== dde3-toolkit v{} run log ===", env!("CARGO_PKG_VERSION"));
                let _ = writeln!(w, "started: {human}");
                let _ = writeln!(w, "mode: {}", if demo { "demo (simulated ECU)" } else { "K-line" });
                let _ = writeln!(w, "{human} DDE3 Toolkit started");
                let _ = w.flush();
                eprintln!("run log: {}", path.display());
                self.run_log_path = path;
                self.run_log = Some(w);
            }
            Err(e) => eprintln!("run log: could not open temp log file: {e}"),
        }
    }

    fn start<F>(&mut self, f: F)
    where
        F: FnOnce(Arc<Mutex<Option<Kline>>>, Sender<Msg>) + Send + 'static,
    {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        self.busy = true;
        self.error = None;
        self.progress = 0.0;
        self.progress_text.clear();
        let link = self.link.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let panic_tx = tx.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(link, tx)));
            if result.is_err() {
                let _ = panic_tx.send(Msg::Error(
                    "internal error in worker thread; operation aborted".into(),
                ));
            }
        });
    }

    fn connect(&mut self) {
        if self.port.is_empty() {
            self.error = Some("No serial port selected".into());
            return;
        }
        self.stop_live();
        // The port always reopens at 9600; a prior fast session is gone.
        self.fast_baud_active = None;
        let path = self.port.clone();
        let link = self.link.clone();
        let tx = self.tx.clone();
        self.busy = true;
        self.error = None;
        self.status = "Opening port".into();
        thread::spawn(move || {
            let mut guard = lock_or_recover(&link);
            match Kline::open(&path, 9600) {
                Ok(mut k) => {
                    let ident = k.identify();
                    match ident {
                        Ok(r) => {
                            // identify() already stripped the `12 <len> A0`
                            // envelope, so the validity check runs on the bare
                            // string — it used to run on the envelope and fired
                            // the warning on every healthy connect.
                            let ident = protocol::parse_ident(&r);
                            if ident.is_none() {
                                let _ = tx.send(Msg::Log(
                                    "WARNING: ident looks erased/corrupt; the application may be incomplete".into(),
                                ));
                            }
                            snapshot_battery(&mut k, &tx);
                            let logs = k.take_logs();
                            *guard = Some(k);
                            for l in logs {
                                let _ = tx.send(Msg::Log(l));
                            }
                            let _ = tx.send(Msg::Connected(EcuInfo {
                                ident,
                                hardware: "DDE3.0".into(),
                            }));
                        }
                        Err(e) => {
                            let logs = k.take_logs();
                            for l in logs {
                                let _ = tx.send(Msg::Log(l));
                            }
                            let _ = tx.send(Msg::Error(format!("ident failed: {e}. Check ECU power, K-line wiring and 9600 8E1 port.")));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("cannot open {path}: {e}")));
                }
            }
        });
    }

    /// Start the automated actions requested on the CLI (auto-write/read/cal/
    /// log/live). Called on connect, or after a fast-baud switch completes.
    fn start_auto_actions(&mut self) {
        if self.auto_write {
            self.tab = Tab::Write;
            self.backup_ok = true;
            self.write_confirm = "WRITE".into();
            self.start_write();
        } else if self.auto_write_cal {
            self.tab = Tab::Write;
            self.backup_ok = true;
            self.write_confirm = "WRITE".into();
            self.start_write_cal();
        } else if self.auto_read {
            self.tab = Tab::Read;
            self.start_read();
        } else if self.auto_cal {
            self.tab = Tab::Read;
            self.start_read_cal();
        } else if let Some(path) = self.auto_log.clone() {
            self.start_logging_to(path);
        } else if self.auto_live {
            self.start_live();
        }
    }

    /// Switch the DS2 link up via service 0x91 (needs the baud patch flashed).
    /// Runs on a worker so the UI stays responsive; the result arrives as
    /// `Msg::FastBaud` / `Msg::FastBaudFailed`. `rate` is the requested target
    /// (from the CLI `--fast-baud`, or 125000 from the Live-tab button).
    fn switch_fast_baud_async(&mut self, rate: u32) {
        if self.live_running {
            self.stop_live();
        }
        self.status = format!("Switching DS2 link to {rate} baud");
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            match guard.as_mut() {
                Some(k) => {
                    let res = k.switch_fast_baud(rate, 9600);
                    for l in k.take_logs() {
                        let _ = tx.send(Msg::Log(l));
                    }
                    match res {
                        Ok(reopen) => {
                            let _ = tx.send(Msg::FastBaud(reopen));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::FastBaudFailed(format!("{e}")));
                        }
                    }
                }
                None => {
                    let _ = tx.send(Msg::FastBaudFailed("link closed".into()));
                }
            }
        });
    }

    fn disconnect(&mut self) {
        if self.busy {
            self.error =
                Some("Wait for the current operation to finish before disconnecting".into());
            return;
        }
        self.stop_live();
        self.connected = false;
        self.fast_baud_active = None;
        // Back to the default (original WinKFP) 9600 timing on the next session.
        self.fast9600.store(false, Ordering::Relaxed);
        self.ecu = None;
        self.status = "Disconnecting".into();
        let link = self.link.clone();
        thread::spawn(move || {
            let mut guard = lock_or_recover(&link);
            if let Some(k) = guard.as_mut() {
                k.recover_link();
            }
            *guard = None;
        });
    }

    // -----------------------------------------------------------------------
    // Flash read
    // -----------------------------------------------------------------------

    fn start_read(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.stop_live();
        self.active_job = Some(Job::Full);
        self.job_started = Some(Instant::now());
        let path = PathBuf::from(self.read_path.clone());
        // At a fast baud the wire is fast enough for back-to-back frames —
        // the 3 ms/byte pacing (a 9600 habit) would dominate the 9-byte
        // request frames (same rule as bench and the live loop).
        let fast = self.fast_baud_active.is_some();
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Reading full flash".into()));
            k.set_silence(10);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Reading {done} / {total} bytes"),
                ));
            };
            k.set_write_delay(if fast { 0 } else { 3 });
            // read_full retries once internally on a non-link error
            // (20 ms silence) — the same recovery the CLI gets.
            let result = k.read_full(&mut progress);
            k.set_write_delay(3);
            let logs = k.take_logs();
            for l in logs {
                let _ = tx.send(Msg::Log(l));
            }
            match result {
                Ok(data) => match std::fs::write(&path, &data) {
                    Ok(()) => {
                        let _ = tx.send(Msg::ReadDone(data, path));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(format!("cannot save {}: {e}", path.display())));
                    }
                },
                Err(e) => {
                    report_error(&tx, "read failed", &e);
                }
            }
        });
    }

    fn start_read_cal(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.stop_live();
        self.active_job = Some(Job::Cal);
        self.job_started = Some(Instant::now());
        let path = PathBuf::from(self.cal_path.clone());
        // Same fast-baud pacing rule as the full read (see start_read).
        let fast = self.fast_baud_active.is_some();
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Reading calibration".into()));
            k.set_silence(10);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Reading calibration {done} / {total} bytes"),
                ));
            };
            k.set_write_delay(if fast { 0 } else { 3 });
            // read_range retries once internally on a non-link error
            // (20 ms silence) — the same recovery the CLI gets.
            let result = k.read_range(CAL_START, CAL_LEN, &mut progress);
            k.set_write_delay(3);
            let logs = k.take_logs();
            for l in logs {
                let _ = tx.send(Msg::Log(l));
            }
            match result {
                Ok(data) => match std::fs::write(&path, &data) {
                    Ok(()) => {
                        let _ = tx.send(Msg::CalDone(path, data.len()));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(format!("cannot save {}: {e}", path.display())));
                    }
                },
                Err(e) => {
                    report_error(&tx, "calibration read failed", &e);
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Flash write
    // -----------------------------------------------------------------------

    fn start_write(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        if !self.backup_ok || self.write_confirm != "WRITE" {
            self.error = Some("Confirm the backup and type WRITE".into());
            return;
        }
        if self.image_is_partial {
            self.error = Some("Full write needs a 256 KB image".into());
            return;
        }
        let Some(image) = self.prepare_write_image() else {
            return;
        };
        // Data frames go out unpaced (0 ms/byte): at 9600 a byte occupies the
        // line for 1.04 ms, so host pacing below that is invisible to the ECU
        // — the wire bitstream is identical, and MPPS (the reference) sends
        // back-to-back. Per-byte pacing only added host time: the real-car
        // capture measured 830 ms/frame at 3 ms/byte vs MPPS's ~330 ms/frame
        // (captures/dde3-20261123-122003.log vs captures/mpps capture/mppswrite.txt).
        self.stop_live();
        self.active_job = Some(Job::Write);
        self.job_started = Some(Instant::now());
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Erasing and writing".into()));
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Writing {done} / {total} bytes"),
                ));
            };
            k.set_silence(10);
            k.set_write_delay(0);
            let result = k.write_full(&image, 9600, 0, &mut progress);
            k.set_write_delay(3);
            let logs = k.take_logs();
            for l in logs {
                let _ = tx.send(Msg::Log(l));
            }
            match result {
                Ok(()) => {
                    let _ = tx.send(Msg::WriteDone);
                }
                Err(e) => {
                    report_error(&tx, "write failed", &e);
                }
            }
        });
    }

    fn start_write_cal(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        if !self.backup_ok || self.write_confirm != "WRITE" {
            self.error = Some("Confirm the backup and type WRITE".into());
            return;
        }
        let Some(image) = self.prepare_write_image() else {
            return;
        };
        // Unpaced data frames, same rule as the full write (see start_write).
        self.stop_live();
        self.active_job = Some(Job::WriteCal);
        self.job_started = Some(Instant::now());
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Erasing and writing calibration".into()));
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Writing calibration {done} / {total} bytes"),
                ));
            };
            k.set_silence(10);
            k.set_write_delay(0);
            let result = k.write_calibration(&image, 9600, 0, &mut progress);
            k.set_write_delay(3);
            let logs = k.take_logs();
            for l in logs {
                let _ = tx.send(Msg::Log(l));
            }
            match result {
                Ok(()) => {
                    let _ = tx.send(Msg::WriteDone);
                }
                Err(e) => {
                    report_error(&tx, "calibration write failed", &e);
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Fault codes (DS2 FS_LESEN / FS_SHADOW_LESEN / FS_LOESCHEN)
    // -----------------------------------------------------------------------

    fn start_dtc_read(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.stop_live();
        self.dtc_clear_armed = false;
        self.active_job = Some(Job::Dtc);
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Reading fault memory".into()));
            match k.read_dtcs() {
                Ok(dtcs) => {
                    let logs = k.take_logs();
                    for l in logs {
                        let _ = tx.send(Msg::Log(l));
                    }
                    let _ = tx.send(Msg::DtcDone(dtcs));
                }
                Err(e) => {
                    let logs = k.take_logs();
                    for l in logs {
                        let _ = tx.send(Msg::Log(l));
                    }
                    report_error(&tx, "fault read failed", &e);
                }
            }
        });
    }

    fn start_dtc_clear(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.stop_live();
        self.dtc_clear_armed = false;
        self.active_job = Some(Job::DtcClear);
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Clearing fault memory".into()));
            match k.clear_faults_ds2() {
                Ok(()) => {
                    let logs = k.take_logs();
                    for l in logs {
                        let _ = tx.send(Msg::Log(l));
                    }
                    let _ = tx.send(Msg::DtcCleared);
                }
                Err(e) => {
                    let logs = k.take_logs();
                    for l in logs {
                        let _ = tx.send(Msg::Log(l));
                    }
                    report_error(&tx, "fault clear failed", &e);
                }
            }
        });
    }

    /// Fault-memory clear after a successful write: the ECU reboots after
    /// `FLASH_SCHREIBEN_ENDE`, so the user power-cycles first, then we wait
    /// 5 s, clear the DS2 fault memory and re-read it.
    fn start_post_write_clear(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.stop_live();
        self.dtc_clear_armed = false;
        self.active_job = Some(Job::DtcClear);
        self.start(move |link, tx| {
            let _ = tx.send(Msg::Status("Waiting 5 s after power cycle".into()));
            thread::sleep(Duration::from_secs(5));
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let mut result = k.clear_faults_ds2();
            if result.is_err() && k.recover_link_for(Duration::from_secs(5)) {
                result = k.clear_faults_ds2();
            }
            match result {
                Ok(()) => {
                    let dtcs = k.read_dtcs().unwrap_or_default();
                    let logs = k.take_logs();
                    for l in logs {
                        let _ = tx.send(Msg::Log(l));
                    }
                    let _ = tx.send(Msg::DtcDone(dtcs));
                }
                Err(e) => report_error(&tx, "post-write fault clear failed", &e),
            }
        });
    }

    /// Battery voltage snapshot (ADF id 0x0F65, scale 0.02037243 / LSB).
    fn start_battery(&mut self) {
        if self.busy {
            self.error = Some("Another operation is still running".into());
            return;
        }
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.active_job = Some(Job::Dtc);
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            match k.live_values(&[(0x0F, 0x65)], true) {
                Ok(vals) => {
                    let raw = vals.first().copied().unwrap_or(0);
                    let _ = tx.send(Msg::Log(format!("BATTERY raw {raw}")));
                    let _ = tx.send(Msg::Battery(raw as f64 * 0.02037243));
                }
                Err(e) => {
                    report_error(&tx, "battery read failed", &e);
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Live data
    // -----------------------------------------------------------------------

    fn start_logging(&mut self) {
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        let ids: Vec<u16> = LIVE_VARS
            .iter()
            .zip(self.live_enabled.iter())
            .filter(|(_, on)| **on)
            .map(|(v, _)| v.id)
            .collect();
        if ids.is_empty() {
            self.error = Some("Select at least one live variable".into());
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(default_log_file_name())
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        self.start_logging_to(path);
    }

    fn start_logging_to(&mut self, path: PathBuf) {
        let ids: Vec<u16> = LIVE_VARS
            .iter()
            .zip(self.live_enabled.iter())
            .filter(|(_, on)| **on)
            .map(|(v, _)| v.id)
            .collect();
        if ids.is_empty() {
            self.error = Some("Select at least one live variable".into());
            return;
        }
        let file = match File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(format!("cannot create {}: {e}", path.display()));
                return;
            }
        };
        let mut w = BufWriter::new(file);
        let _ = write!(w, "t_s");
        for id in &ids {
            if let Some(v) = LIVE_VARS.iter().find(|v| v.id == *id) {
                let _ = write!(w, ",{}[{}]", v.name, v.unit);
            }
        }
        let _ = writeln!(w);
        let _ = w.flush();
        *lock_or_recover(&self.live_log) = Some(LogWriter {
            file: w,
            path: path.clone(),
            rows: 0,
            start: Instant::now(),
            ids,
        });
        self.logging = true;
        self.log_path = path.display().to_string();
        self.log_rows = 0;
        self.status = "Logging to CSV".into();
        if !self.live_running {
            self.start_live();
        }
    }

    fn stop_logging(&mut self) {
        let taken = lock_or_recover(&self.live_log).take();
        if let Some(mut w) = taken {
            let _ = w.file.flush();
            self.log_rows = w.rows;
            self.log_path = w.path.display().to_string();
            self.status = format!("Log saved: {} ({} rows)", w.path.display(), w.rows);
        }
        self.logging = false;
    }

    fn start_live(&mut self) {
        if !self.connected || self.busy || self.live_running {
            return;
        }
        let ids: Vec<u16> = LIVE_VARS
            .iter()
            .zip(self.live_enabled.iter())
            .filter(|(_, on)| **on)
            .map(|(v, _)| v.id)
            .collect();
        if ids.is_empty() {
            self.error = Some("Select at least one live variable".into());
            return;
        }
        self.live_running = true;
        self.live_stop = Arc::new(AtomicBool::new(false));
        let stop = self.live_stop.clone();
        let link = self.link.clone();
        let tx = self.tx.clone();
        let log = self.live_log.clone();
        // At a fast baud the wire byte time is ~0.08 ms, so any per-byte pacing
        // dominates TX — drop it to 0. At 9600 the default is the original
        // WinKFP timing (3 ms/byte); the "faster 9600" switch (Live tab) drops
        // it to 1 ms (≈ the natural byte time). Shared atomic: the toggle can
        // be flipped while this loop is running.
        let fast = self.fast_baud_active.is_some();
        let fast9600 = self.fast9600.clone();
        thread::spawn(move || {
            let mut reported = false;
            // Cycle-completion stamps for the Hz counter. The rate must be
            // measured HERE, where the cycle time is real: the UI thread
            // processes Msg::Live in frame-sized bursts (~60 fps), so a
            // consumer-side dt spikes to ~1/dt≈∞ whenever two messages land
            // in one frame — the old counter read "unrealistically high" at
            // fast baud for exactly that reason. A 1 s sliding window gives
            // the true mean rate (n-1 intervals over the span): no overshoot
            // by construction, and it converges within ~1 s of starting.
            let mut cycles: VecDeque<Instant> = VecDeque::new();
            while !stop.load(Ordering::Relaxed) {
                let (pairs, logs, link_error) = {
                    let mut guard = lock_or_recover(&link);
                    match guard.as_mut() {
                        Some(k) => {
                            let mut pairs = Vec::new();
                            let mut error = None;
                            // Live-logging pacing (Tier 0, docs/FINDINGS_faster_logger.md):
                            // 1 ms/byte ≈ the natural 9600 baud byte time
                            // (1.04 ms), so the frame goes out as one
                            // continuous stream — exactly as fast as the ECU
                            // itself transmits, no new wire behaviour. The
                            // 3 ms default was an inherited WinKFP habit.
                            k.set_write_delay(if fast {
                                0
                            } else if fast9600.load(Ordering::Relaxed) {
                                1
                            } else {
                                3
                            });
                            // DDE3 live data via MW_SELECT_LESEN (`12 <LEN> 0B 10 <id bytes>`).
                            // The selected set is batched into as FEW telegrams as the
                            // 0x80-byte DS2 cap allows (protocol::MW_BATCH_SIZE) — a
                            // normal live set is a single telegram, exactly like the
                            // tested WinKFP logger (FINDINGS §2.2).
                            for chunk in ids.chunks(protocol::MW_BATCH_SIZE) {
                                let addrs: Vec<(u8, u8)> = chunk
                                    .iter()
                                    .map(|id| ((*id >> 8) as u8, *id as u8))
                                    .collect();
                                match k.live_values(&addrs, false) {
                                    Ok(vals) => {
                                        for (id, val) in chunk.iter().zip(vals) {
                                            pairs.push((*id, val));
                                        }
                                    }
                                    Err(e) => {
                                        error = Some(e);
                                        break;
                                    }
                                }
                            }
                            let fatal = error.as_ref().is_some_and(protocol::is_link_error);
                            (
                                error.is_none().then_some(pairs),
                                k.take_logs(),
                                error.map(|e| (format!("{e}"), fatal)),
                            )
                        }
                        None => (None, Vec::new(), None),
                    }
                };
                for l in logs {
                    let _ = tx.send(Msg::Log(l));
                }
                if let Some((message, fatal)) = link_error {
                    if fatal {
                        let _ = tx.send(Msg::LinkLost(format!("live polling: {message}")));
                        break;
                    }
                    if !reported {
                        reported = true;
                        let _ = tx.send(Msg::Log(format!("LIVE ERROR: {message}")));
                    }
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                if let Some(v) = pairs {
                    reported = false;
                    let hz = live_rate_update(&mut cycles, Instant::now());
                    let mut rows = None;
                    if let Some(w) = lock_or_recover(&log).as_mut() {
                        w.write_row(&v);
                        rows = Some(w.rows);
                    }
                    if let Some(rows) = rows {
                        let _ = tx.send(Msg::LogProgress(rows));
                    }
                    let _ = tx.send(Msg::Live(v, hz));
                }
                // Poll gap: default 55 ms — the original WinKFP tester gap.
                // Back-to-back (1 ms) was faster but is flaky in the car, so it
                // is opt-in via the "faster 9600" switch; an active fast-baud
                // link (patched ECU) always polls back-to-back. The transaction
                // itself still costs ~50–100 ms of wire + silence time.
                let gap_ms = if fast || fast9600.load(Ordering::Relaxed) { 1 } else { 55 };
                thread::sleep(Duration::from_millis(gap_ms));
            }
        });
    }

    fn stop_live(&mut self) {
        if self.logging {
            self.stop_logging();
        }
        self.live_stop.store(true, Ordering::Relaxed);
        self.live_running = false;
    }

    // -----------------------------------------------------------------------
    // Image loading / checksum
    // -----------------------------------------------------------------------

    fn load_image(&mut self, path: PathBuf) {
        match std::fs::read(&path) {
            Ok(data) if data.len() == IMAGE_SIZE => {
                self.image_is_partial = false;
                self.checks = checksum::verify(&data);
                self.image = Some(data);
                self.image_name = path.display().to_string();
                self.error = None;
                self.status = "Full image loaded".into();
                self.sync_comp_edits();
            }
            Ok(data) if data.len() == CAL_LEN => {
                self.image_is_partial = true;
                self.checks = checksum::verify(&pad_calibration(&data));
                self.image = Some(data);
                self.image_name = path.display().to_string();
                self.error = None;
                self.status = "Calibration image loaded".into();
                self.sync_comp_edits();
            }
            Ok(data) => {
                self.error = Some(format!(
                    "Image is {} bytes; expected {} (full) or {} (calibration)",
                    data.len(),
                    IMAGE_SIZE,
                    CAL_LEN
                ));
            }
            Err(e) => self.error = Some(format!("Cannot read {}: {e}", path.display())),
        }
    }

    fn sync_comp_edits(&mut self) {
        let partial = self.image_is_partial;
        let image = &self.image;
        let edits: Vec<String> = self
            .checks
            .iter()
            .map(|c| {
                let Some(img) = image else {
                    return String::new();
                };
                let off = if partial {
                    c.pos.saturating_sub(CAL_START)
                } else {
                    c.pos
                };
                if off + 4 <= img.len() {
                    format!(
                        "{:02X}{:02X}{:02X}{:02X}",
                        img[off],
                        img[off + 1],
                        img[off + 2],
                        img[off + 3]
                    )
                } else {
                    String::new()
                }
            })
            .collect();
        self.comp_edit = edits;
    }

    fn refresh_checks(&mut self) {
        if let Some(img) = &self.image {
            self.checks = if self.image_is_partial {
                checksum::verify(&pad_calibration(img))
            } else {
                checksum::verify(img)
            };
        }
        self.sync_comp_edits();
    }

    /// Checksums to display for the loaded image — always exactly the blocks
    /// discovered by `find_checksums` (a 32 KB read yields the calibration
    /// block only, a 256 KB read yields calibration + code when present).
    fn visible_checks(&self) -> Vec<checksum::Checksum> {
        self.checks.clone()
    }

    fn checks_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// File offset of a discovered checksum inside the *loaded* image (a
    /// 32 KB image holds the calibration block 0x38000 earlier than the
    /// padded view used for verification).
    fn file_offset_of(&self, pos: usize) -> usize {
        if self.image_is_partial {
            pos.saturating_sub(CAL_START)
        } else {
            pos
        }
    }

    fn apply_comp_edit(&mut self, idx: usize) {
        let partial = self.image_is_partial;
        let Some(cs) = self.checks.get(idx).copied() else {
            return;
        };
        let raw: String = self
            .comp_edit
            .get(idx)
            .map(|s| s.chars().filter(|c| c.is_ascii_hexdigit()).collect())
            .unwrap_or_default();
        if raw.len() != 8 {
            self.error = Some("Checksum edit must be 8 hex digits".into());
            return;
        }
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).unwrap_or(0);
        }
        let Some(mut img) = self.image.clone() else {
            return;
        };
        let off = if partial {
            cs.pos.saturating_sub(CAL_START)
        } else {
            cs.pos
        };
        if off + 4 > img.len() {
            self.error = Some("Checksum offset is outside the image".into());
            return;
        }
        img[off..off + 4].copy_from_slice(&bytes);
        self.image = Some(img);
        self.refresh_checks();
        self.error = None;
        self.status = format!(
            "Applied manual {} checksum {:08X}",
            cs.name,
            u32::from_le_bytes(bytes)
        );
    }

    fn correct_block_checksum(&mut self, idx: usize) {
        let partial = self.image_is_partial;
        let Some(cs) = self.checks.get(idx).copied() else {
            return;
        };
        let Some(img) = self.image.clone() else {
            return;
        };
        let fixed = if partial {
            let full = pad_calibration(&img);
            checksum::correct_at(&full, cs.pos).map(|f| f[CAL_START..CAL_START + CAL_LEN].to_vec())
        } else {
            checksum::correct_at(&img, cs.pos)
        };
        match fixed {
            Some(f) => {
                self.image = Some(f);
                self.refresh_checks();
                self.status = format!("Corrected {} checksum", cs.name);
            }
            None => self.error = Some(format!("No {} checksum found in the image", cs.name)),
        }
    }

    fn correct_all_checksums(&mut self) {
        let partial = self.image_is_partial;
        let Some(img) = self.image.clone() else {
            return;
        };
        let fixed = if partial {
            let full = pad_calibration(&img);
            checksum::correct(&full)[CAL_START..CAL_START + CAL_LEN].to_vec()
        } else {
            checksum::correct(&img)
        };
        self.image = Some(fixed);
        self.refresh_checks();
        self.status = "Corrected all checksums".into();
    }

    /// Open-image dialog. One entry point for the buttons, the Overview quick
    /// action and the Ctrl+O shortcut so they cannot drift apart.
    fn open_image_dialog(&mut self) {
        if self.busy {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("ECU image", &["bin"])
            .pick_file()
        {
            self.load_image(path);
        }
    }

    /// Drop the loaded image from the toolkit. Purely UI state — nothing is
    /// sent to the ECU. Refused while an operation is running, because the
    /// worker may still be using the image.
    fn unload_image(&mut self) {
        if self.busy {
            self.error =
                Some("Cannot unload the image while an operation is running.".into());
            return;
        }
        self.image = None;
        self.image_name.clear();
        self.image_is_partial = false;
        self.checks.clear();
        self.comp_edit.clear();
        self.patch_notice = None;
        self.patch_target_ok = None;
        self.status = "Image unloaded".into();
        self.push_log("IMAGE unloaded".into());
    }

    /// The loaded-image status strip (name, size/kind, checksum state) plus the
    /// Close control. Returns true when the user asked to unload the image.
    fn image_status_row(&mut self, ui: &mut Ui) -> bool {
        let Some(size) = self.image.as_ref().map(|i| i.len()) else {
            return false;
        };
        let name = self.image_name.clone();
        let partial = self.image_is_partial;
        let ok = self.checks_ok();
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Loaded").size(11.0).color(MUTED));
            let shown = if name.is_empty() {
                "(unnamed image)".to_string()
            } else {
                name.clone()
            };
            ui.label(RichText::new(short_name(&shown, 46)).size(12.0).color(TEXT))
                .on_hover_text(&shown);
            pill(
                ui,
                &format!("{size} B · {}", if partial { "calibration" } else { "full" }),
                ACCENT,
            );
            if ok {
                pill(ui, "checksums OK", OK);
            } else {
                pill(ui, "checksums need correction", WARN);
            }
            if centered_button_enabled(
                ui,
                !self.busy,
                Vec2::new(120.0, 26.0),
                egui::Button::new("Close image"),
            )
            .on_disabled_hover_text("An operation is running — wait for it to finish")
            .on_hover_text("Unload the image from the toolkit (the ECU is not touched)")
            .clicked()
            {
                close = true;
            }
        });
        close
    }

    fn save_image(&mut self) {
        let Some(img) = self.image.as_ref() else {
            return;
        };
        let name = if self.image_is_partial {
            "dde3_cal_corrected.bin"
        } else {
            "dde3_corrected.bin"
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(name)
            .add_filter("BIN", &["bin"])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, img) {
            Ok(()) => self.status = format!("Saved {}", path.display()),
            Err(e) => self.error = Some(format!("Cannot save {}: {e}", path.display())),
        }
    }

    fn prepare_write_image(&mut self) -> Option<Vec<u8>> {
        let Some(image) = self.image.clone() else {
            self.error = Some("Load an image first".into());
            return None;
        };
        if self.image_is_partial {
            if image.len() != CAL_LEN {
                self.error = Some("Calibration image must be exactly 32 KB".into());
                return None;
            }
            let full = pad_calibration(&image);
            if checksum::verify(&full).is_empty() {
                self.error = Some("No EDC15M TDI V3.1 checksum block found in the image".into());
                return None;
            }
            let fixed = checksum::correct(&full);
            let corrected = fixed[CAL_START..CAL_START + CAL_LEN].to_vec();
            let changed = corrected != image;
            self.image = Some(corrected.clone());
            self.checks = checksum::verify(&pad_calibration(&corrected));
            self.sync_comp_edits();
            if changed {
                self.status = "Calibration checksum corrected automatically".into();
            }
            return Some(corrected);
        }
        if image.len() != IMAGE_SIZE {
            self.error = Some("Image must be exactly 256 KB".into());
            return None;
        }
        let corrected = checksum::correct(&image);
        let changed = corrected != image;
        self.image = Some(corrected.clone());
        self.checks = checksum::verify(&corrected);
        self.sync_comp_edits();
        if changed {
            self.status = "Checksums corrected automatically".into();
        }
        Some(corrected)
    }

    // -----------------------------------------------------------------------
    // UI
    // -----------------------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(18, 12))
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("DDE3")
                                .font(FontId::proportional(24.0))
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new("Toolkit · flash & diagnostics")
                                .size(12.0)
                                .color(MUTED),
                        );
                    });
                    ui.add_space(26.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label(RichText::new("Port").size(12.0).color(MUTED));
                    egui::ComboBox::from_id_salt("port")
                        .selected_text(if self.port.is_empty() {
                            "no port"
                        } else {
                            &self.port
                        })
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for p in &self.ports {
                                ui.selectable_value(&mut self.port, p.clone(), p);
                            }
                        });
                    if ui
                        .add(egui::Button::new("⟳").frame(false))
                        .on_hover_text("Refresh serial ports")
                        .clicked()
                    {
                        self.refresh_ports();
                    }
                    if self.connected {
                        if centered_button_enabled(
                            ui,
                            !self.busy,
                            Vec2::new(100.0, 30.0),
                            egui::Button::new(RichText::new("Disconnect").color(DANGER))
                                .fill(CARD_ALT),
                        )
                        .on_disabled_hover_text("An operation is running")
                        .on_hover_text("Close the K-line link")
                        .clicked()
                        {
                            self.disconnect();
                        }
                    } else if centered_button_enabled(
                        ui,
                        !self.busy && !self.port.is_empty(),
                        Vec2::new(100.0, 30.0),
                        egui::Button::new(RichText::new("Connect").color(BG)).fill(ACCENT),
                    )
                    .on_disabled_hover_text(if self.port.is_empty() {
                        "Select a serial port first"
                    } else {
                        "An operation is running"
                    })
                    .on_hover_text("Open the K-line link and identify the ECU")
                    .clicked()
                    {
                        self.connect();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (dot, text, col) = if self.busy {
                            ("●", "BUSY", WARN)
                        } else if self.connected {
                            ("●", "CONNECTED", OK)
                        } else {
                            ("●", "OFFLINE", MUTED)
                        };
                        ui.label(RichText::new(text).size(11.0).color(col).strong());
                        ui.label(RichText::new(dot).color(col));
                        // Global battery pill (was on the Read/Write tabs): a
                        // weak battery mid-flash is the classic brick cause.
                        battery_pill(ui, self.battery_v);
                        if !self.status.is_empty() {
                            ui.label(RichText::new(&self.status).size(12.0).color(MUTED));
                        }
                    });
                });
            });
    }

    fn side_nav(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(196.0)
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(12, 14)),
            )
            .show(ctx, |ui| {
                // The tab list scrolls on its own; the LINK · GLOBAL, IMAGE
                // and ECU/version sections below are pinned and always
                // visible. The pinned content is measured with an invisible
                // sizing pass each frame, so the tab area is capped at
                // exactly the remaining height — nothing gets clipped, and
                // no reserved-height constant can drift from the real
                // content.
                let origin = ui.cursor().min;
                let avail = ui.available_size();
                let pinned_h = ui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .id_salt("nav-pinned-measure")
                            .max_rect(egui::Rect::from_min_size(origin, avail))
                            .invisible(),
                        |ui| {
                            self.pinned_sections(ui);
                        },
                    )
                    .response
                    .rect
                    .height();
                let tab_h = (avail.y - pinned_h).max(80.0);
                ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        origin,
                        Vec2::new(avail.x, tab_h),
                    )),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("nav-tabs")
                            .show(ui, |ui| {
                                for tab in [
                    Tab::Overview,
                    Tab::Read,
                    Tab::Write,
                    Tab::Checksum,
                    Tab::Patches,
                    Tab::Dtc,
                    Tab::Live,
                    Tab::Logger,
                    Tab::Log,
                ] {
                    let selected = self.tab == tab;
                    let text = RichText::new(format!("{}   {}", tab.glyph(), tab.label()))
                        .size(14.0)
                        .color(if selected { TEXT } else { MUTED });
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            CARD_ALT
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0_f32, BORDER)
                        } else {
                            Stroke::NONE
                        })
                        .min_size(Vec2::new(172.0, 30.0));
                    if ui.add(button).clicked() {
                        self.tab = tab;
                        if tab != Tab::Live {
                            self.stop_live();
                        }
                    }
                    ui.add_space(2.0);
                }
                        });
                    },
                );
                // Pinned sections (LINK · GLOBAL, IMAGE, ECU/version),
                // rendered for real at the bottom of the rail.
                ui.allocate_new_ui(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        pos2(origin.x, origin.y + tab_h),
                        Vec2::new(avail.x, pinned_h),
                    )),
                    |ui| {
                        self.pinned_sections(ui);
                    },
                );
            });
    }

    /// The pinned part of the side rail: LINK · GLOBAL toggles, the loaded
    /// image, and the ECU/version block. Rendered twice per frame — once
    /// invisibly (sizing pass) to measure its height, once for real at the
    /// bottom of the rail.
    fn pinned_sections(&mut self, ui: &mut Ui) {
                // LINK — program-wide link/timing toggles. They used to live
                // on the Live tab (faster 9600, fast baud) and the Write tab;
                // they affect the whole app, so they are global.
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("LINK · GLOBAL").size(9.0).color(MUTED));
                let mut fast9600 = self.fast9600.load(Ordering::Relaxed);
                if ui
                    .checkbox(&mut fast9600, RichText::new("faster 9600").size(11.0))
                    .on_hover_text(
                        "Back-to-back polling (1 ms gap) + 1 ms/byte TX pacing instead of the default 55 ms / 3 ms. Applies immediately.",
                    )
                    .changed()
                {
                    self.fast9600.store(fast9600, Ordering::Relaxed);
                    self.push_log(if fast9600 {
                        "FAST9600 on — back-to-back, 1 ms/byte".into()
                    } else {
                        "FAST9600 off — 55 ms poll gap, 3 ms/byte".into()
                    });
                }
                match self.fast_baud_active {
                    Some(rate) => {
                        ui.label(
                            RichText::new(format!("● fast: {rate} baud"))
                                .size(11.0)
                                .color(ACCENT)
                                .strong(),
                        );
                    }
                    None => {
                        let mut fast_baud = false;
                        let resp = ui.add_enabled(
                            self.connected && !self.busy && !self.live_running,
                            egui::Checkbox::new(&mut fast_baud, RichText::new("fast baud 125k").size(11.0)),
                        );
                        if resp
                            .on_hover_text(
                                "Switch the DS2 link up via service 0x91 (needs the baud patch flashed). Reverts on power cycle / reconnect.",
                            )
                            .changed()
                        {
                            self.switch_fast_baud_async(125_000);
                        }
                    }
                }
                if self.image.is_some() {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.label(RichText::new("IMAGE").size(9.0).color(MUTED));
                    let name = if self.image_name.is_empty() {
                        "(unnamed)".to_string()
                    } else {
                        self.image_name.clone()
                    };
                    ui.label(RichText::new(short_name(&name, 22)).size(11.0).color(ACCENT))
                        .on_hover_text(&name);
                    ui.label(
                        RichText::new(format!(
                            "{} B · {}",
                            self.image.as_ref().map(|i| i.len()).unwrap_or(0),
                            if self.image_is_partial { "cal" } else { "full" }
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                }
                // ECU info + version — a normal trailing section (was a
                // bottom-up block pinned to the panel bottom, which left no
                // room for the LINK section once the image was loaded).
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                if let Some(ecu) = &self.ecu {
                    ui.label(RichText::new("ECU").size(9.0).color(MUTED));
                    if let Some(i) = &ecu.ident {
                        ui.label(
                            RichText::new(format!("SW {}", i.software))
                                .size(12.0)
                                .color(ACCENT),
                        );
                        ui.label(
                            RichText::new(format!("HW {}", i.hw_number))
                                .size(10.0)
                                .color(MUTED),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("HW {}", ecu.hardware))
                                .size(10.0)
                                .color(MUTED),
                        );
                    }
                }
                ui.add_space(6.0);
                // Version comes from Cargo.toml (bumped by scripts/release.sh),
                // so the GUI label can never drift from the release again.
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.0)
                        .color(MUTED),
                );
                ui.label(RichText::new("EDC15M/DDE3").size(11.0).color(MUTED));
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                if let Some(err) = self.error.clone() {
                    Frame::new()
                        .fill(Color32::from_rgb(55, 24, 26))
                        .stroke(Stroke::new(1.0_f32, DANGER))
                        .corner_radius(10)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("!").color(DANGER).strong());
                                ui.label(RichText::new(err).color(TEXT));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("dismiss").clicked() {
                                        self.error = None;
                                    }
                                });
                            });
                        });
                    ui.add_space(10.0);
                }
                if self.tab == Tab::Log {
                    self.log_tab(ui);
                } else {
                    ScrollArea::vertical().show(ui, |ui| match self.tab {
                        Tab::Overview => self.overview(ui),
                        Tab::Read => self.read_tab(ui),
                        Tab::Write => self.write_tab(ui),
                        Tab::Checksum => self.checksum_tab(ui),
                        Tab::Patches => self.patches_tab(ui),
                        Tab::Dtc => self.dtc_tab(ui),
                        Tab::Live => self.live_tab(ui),
                        Tab::Logger => self.logger_tab(ui),
                        Tab::Log => {}
                    });
                }
                self.var_picker_window(ui.ctx());
            });
    }

    fn overview(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Overview",
            "ECU state, quick actions and safety status.",
        );
        ui.add_space(8.0);
        let width = ui.available_width();
        card(ui, width, |ui| {
            self.ecu_card(ui);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);
            self.quick_card(ui);
        });
        ui.add_space(14.0);
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Safety");
            ui.label(RichText::new("Reads are verified. A full write erases program and calibration. Keep a verified backup and stable supply.").size(12.0).color(MUTED));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                pill(ui, "Program writable", WARN);
                pill(ui, "Calibration writable", WARN);
                pill(ui, "Checksum required", ACCENT);
            });
        });
    }

    fn ecu_card(&mut self, ui: &mut Ui) {
        section_title(ui, "ECU");
        if let Some(ecu) = &self.ecu {
            egui::Grid::new("ecu-info")
                .num_columns(2)
                .spacing([12.0, 5.0])
                .show(ui, |ui| {
                    if let Some(i) = &ecu.ident {
                        ui.label(RichText::new("Software").size(11.0).color(MUTED));
                        ui.label(RichText::new(&i.software).size(12.0).color(TEXT));
                        ui.end_row();
                        ui.label(RichText::new("ECU code").size(11.0).color(MUTED));
                        ui.label(
                            RichText::new(protocol::format_pair(&i.ecu_code))
                                .size(12.0)
                                .color(TEXT),
                        );
                        ui.end_row();
                        ui.label(RichText::new("SW date").size(11.0).color(MUTED));
                        ui.label(
                            RichText::new(format!(
                                "{} (week/yr)",
                                protocol::format_pair(&i.date)
                            ))
                            .size(12.0)
                            .color(TEXT),
                        );
                        ui.end_row();
                        ui.label(RichText::new("HW number").size(11.0).color(MUTED));
                        ui.label(RichText::new(&i.hw_number).size(12.0).color(TEXT));
                        ui.end_row();
                        ui.label(RichText::new("G code").size(11.0).color(MUTED));
                        ui.label(RichText::new(&i.g_code).size(12.0).color(TEXT));
                        ui.end_row();
                    }
                    ui.label(RichText::new("Hardware").size(11.0).color(MUTED));
                    ui.label(RichText::new(&ecu.hardware).size(12.0).color(TEXT));
                    ui.end_row();
                    ui.label(RichText::new("Link").size(11.0).color(MUTED));
                    let link_label = match self.fast_baud_active {
                        Some(rate) => format!("K-line · {rate} 8E1"),
                        None => "K-line · 9600 8E1".to_string(),
                    };
                    ui.label(RichText::new(link_label).size(12.0).color(TEXT));
                    ui.end_row();
                    ui.label(RichText::new("Port").size(11.0).color(MUTED));
                    ui.label(RichText::new(&self.port).size(12.0).color(TEXT));
                    ui.end_row();
                });
        } else {
            ui.label(RichText::new("Not connected").color(MUTED));
            ui.add_space(8.0);
            ui.label(
                RichText::new("Select a port and press Connect. The ECU must be powered.")
                    .size(12.0)
                    .color(MUTED),
            );
        }
    }

    fn quick_card(&mut self, ui: &mut Ui) {
        section_title(ui, "Quick actions");
        ui.horizontal_wrapped(|ui| {
            if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(180.0, 34.0),
                egui::Button::new(RichText::new("Read full flash").color(BG)).fill(ACCENT),
            )
            .clicked()
            {
                self.tab = Tab::Read;
                self.start_read();
            }
            if centered_button_enabled(
                ui,
                !self.busy,
                Vec2::new(180.0, 34.0),
                egui::Button::new("Load image…"),
            )
            .on_hover_text("Open a 256 KB full or 32 KB calibration .bin (Ctrl+O)")
            .clicked()
            {
                self.open_image_dialog();
            }
            if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(180.0, 34.0),
                egui::Button::new("Live data"),
            )
            .clicked()
            {
                self.tab = Tab::Live;
                self.start_live();
            }
            if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(180.0, 34.0),
                egui::Button::new("Fault codes"),
            )
            .clicked()
            {
                self.tab = Tab::Dtc;
                self.start_dtc_read();
            }
        });
    }

    fn read_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Read flash", "Full 256 KB read over K-line, 9600 baud.");
        ui.add_space(8.0);
        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save to").color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.read_path).desired_width(420.0));
                if centered_button(ui, Vec2::new(90.0, 28.0), egui::Button::new("Browse…"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("dde3_full.bin")
                        .add_filter("BIN", &["bin"])
                        .save_file()
                    {
                        self.read_path = path.display().to_string();
                    }
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if centered_button_enabled(
                    ui,
                    self.connected && !self.busy,
                    Vec2::new(180.0, 34.0),
                    egui::Button::new(RichText::new("Read full flash").color(BG)).fill(ACCENT),
                )
                .on_disabled_hover_text(if self.connected {
                    "An operation is running"
                } else {
                    "Connect to the ECU first"
                })
                .on_hover_text("Read all 256 KB into memory and save to the path above")
                .clicked()
                {
                    self.start_read();
                }
                if self.active_job == Some(Job::Full) {
                    ui.label(RichText::new(&self.progress_text).color(MUTED));
                }
            });
            if self.active_job == Some(Job::Full) {
                ui.add_space(8.0);
                progress_with_timer(ui, self.progress, self.job_started);
            }
        });
        ui.add_space(14.0);
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Calibration read (32 KB @ 0x38000)");
            ui.label(
                RichText::new("Reads file 0x38000-0x3FFFF without touching the boot area.")
                    .size(11.0)
                    .color(MUTED),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save to").color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.cal_path).desired_width(380.0));
                if centered_button(ui, Vec2::new(90.0, 28.0), egui::Button::new("Browse…"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("dde3_cal.bin")
                        .add_filter("BIN", &["bin"])
                        .save_file()
                    {
                        self.cal_path = path.display().to_string();
                    }
                }
            });
            ui.add_space(8.0);
            if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(180.0, 34.0),
                egui::Button::new(RichText::new("Read calibration").color(BG)).fill(ACCENT),
            )
            .on_disabled_hover_text(if self.connected {
                "An operation is running"
            } else {
                "Connect to the ECU first"
            })
            .on_hover_text("Read file 0x38000-0x3FFFF only")
            .clicked()
            {
                self.start_read_cal();
            }
            if self.active_job == Some(Job::Cal) {
                ui.add_space(8.0);
                progress_with_timer(ui, self.progress, self.job_started);
                ui.label(RichText::new(&self.progress_text).size(11.0).color(MUTED));
            }
            if let Some((path, size)) = &self.cal_info {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Last calibration: {path} ({size} bytes)"))
                        .size(11.0)
                        .color(OK),
                );
            }
        });
        ui.add_space(14.0);
        let checks = self.visible_checks();
        if !checks.is_empty() {
            checksum_card(ui, &checks);
        }
    }

    fn write_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Write flash",
            "Erase and program flash, or calibration only.",
        );
        ui.add_space(8.0);
        let partial = self.image_is_partial;
        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Image").color(MUTED));
                if self.image.is_some() {
                    let name = if self.image_name.is_empty() {
                        "(unnamed image)".to_string()
                    } else {
                        self.image_name.clone()
                    };
                    ui.label(RichText::new(short_name(&name, 52)).size(12.0).color(TEXT))
                        .on_hover_text(&name);
                } else {
                    ui.label(
                        RichText::new("no image loaded")
                            .size(12.0)
                            .color(MUTED),
                    );
                }
                if centered_button_enabled(
                    ui,
                    !self.busy,
                    Vec2::new(90.0, 28.0),
                    egui::Button::new("Open…"),
                )
                .on_hover_text("Open a 256 KB full or 32 KB calibration .bin (Ctrl+O)")
                .clicked()
                {
                    self.open_image_dialog();
                }
                if self.image.is_some()
                    && centered_button_enabled(
                        ui,
                        !self.busy,
                        Vec2::new(90.0, 28.0),
                        egui::Button::new("Close"),
                    )
                    .on_disabled_hover_text("An operation is running")
                    .on_hover_text("Unload the image from the toolkit")
                    .clicked()
                {
                    self.unload_image();
                }
            });
            if let Some(img) = &self.image {
                ui.add_space(6.0);
                let ok = self.checks_ok();
                let kind = if partial {
                    "calibration image"
                } else {
                    "full image"
                };
                ui.label(
                    RichText::new(format!(
                        "{} bytes · {} · {}",
                        img.len(),
                        kind,
                        if ok {
                            "checksum OK"
                        } else {
                            "checksum BAD (corrected automatically on write)"
                        }
                    ))
                    .size(12.0)
                    .color(if ok { MUTED } else { WARN }),
                );
                if !ok
                    && centered_button(ui, Vec2::new(130.0, 30.0), egui::Button::new("Review"))
                        .clicked()
                {
                    self.tab = Tab::Checksum;
                }
            }
            ui.add_space(12.0);
            Frame::new()
                .fill(Color32::from_rgb(45, 36, 20))
                .stroke(Stroke::new(1.0_f32, WARN))
                .corner_radius(10)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.label(RichText::new("Experimental write").color(WARN).strong().size(13.0));
                    ui.label(RichText::new("Full write erases program and calibration. Partial write erases calibration only. Checksums are corrected automatically before writing. Do not interrupt power.").size(12.0).color(TEXT));
                });
            ui.add_space(8.0);
            ui.checkbox(
                &mut self.backup_ok,
                "I have a verified full backup of this ECU",
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Type WRITE to confirm").color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.write_confirm).desired_width(120.0));
            });
            ui.add_space(8.0);
            let ready =
                self.backup_ok && self.write_confirm == "WRITE" && self.image.is_some();
            ui.horizontal_wrapped(|ui| {
                if centered_button_enabled(
                    ui,
                    ready && !partial && self.connected && !self.busy,
                    Vec2::new(180.0, 34.0),
                    egui::Button::new(RichText::new("Erase and write").color(BG)).fill(WARN),
                )
                .on_disabled_hover_text(write_disabled_reason(
                    self.image.is_some(),
                    partial,
                    self.connected,
                    self.busy,
                    ready,
                ))
                .on_hover_text("Erase program + calibration and write the full image")
                .clicked()
                {
                    self.start_write();
                }
                if centered_button_enabled(
                    ui,
                    ready && self.connected && !self.busy,
                    Vec2::new(180.0, 34.0),
                    egui::Button::new(RichText::new("Write calibration only").color(BG))
                        .fill(ACCENT),
                )
                .on_disabled_hover_text(write_disabled_reason(
                    self.image.is_some(),
                    false,
                    self.connected,
                    self.busy,
                    ready,
                ))
                .on_hover_text("Erase + write file 0x38000-0x3FFFF only")
                .clicked()
                {
                    self.start_write_cal();
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(if partial {
                    "Calibration image loaded: only file 0x38000-0x3FFFF will be written."
                } else {
                    "Partial write programs file 0x38000-0x3FFFF only."
                })
                .size(11.0)
                .color(MUTED),
            );
            if matches!(self.active_job, Some(Job::Write | Job::WriteCal)) {
                ui.add_space(8.0);
                progress_with_timer(ui, self.progress, self.job_started);
                ui.label(RichText::new(&self.progress_text).size(12.0).color(MUTED));
            }
        });
        ui.add_space(14.0);
        let checks = self.visible_checks();
        if !checks.is_empty() {
            checksum_card(ui, &checks);
        }
    }

    // -----------------------------------------------------------------------
    // Patches tab — firmware patch manifests (baud 0x91, EWS delete)
    // -----------------------------------------------------------------------

    /// Patch state for the loaded image. Patches need a full 256 KB image; a
    /// 32 KB calibration read is rejected rather than sliced out of bounds.
    fn patch_state(&self, p: &Patch) -> PatchState {
        match &self.image {
            Some(img) if !self.image_is_partial => p.state(img),
            Some(_) => PatchState::Invalid("a patch needs a 256 KB full image".into()),
            None => PatchState::Invalid("load a 256 KB image first".into()),
        }
    }

    fn patches_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Patches",
            "Apply or revert firmware patches on the loaded image. Nothing is sent to the ECU here.",
        );
        ui.add_space(8.0);
        if self.image.is_some() {
            if self.image_status_row(ui) {
                self.unload_image();
            }
            ui.add_space(6.0);
        }
        card(ui, ui.available_width(), |ui| {
            ui.label(
                RichText::new(
                    "Patches edit the image in memory only. Review the changed bytes, save the \
                     image, then write it from the Write flash tab. Every apply/revert also \
                     corrects the TDI V3.1 slots and the cold-start program chain. Code patches \
                     are experimental unless the manifest says they were hardware-tested.",
                )
                .size(12.0)
                .color(WARN),
            );
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized(Vec2::new(150.0, 30.0), egui::Button::new("Open image…"))
                .on_hover_text("Open a 256 KB full read to apply patches to (Ctrl+O)")
                .clicked()
            {
                self.open_image_dialog();
            }
            if ui
                .add_sized(Vec2::new(150.0, 30.0), egui::Button::new("Load manifest…"))
                .on_hover_text("Load an extra patch JSON manifest from disk")
                .clicked()
            {
                self.open_patch_dialog();
            }
            if ui
                .add_sized(Vec2::new(110.0, 30.0), egui::Button::new("Reload"))
                .on_hover_text("Re-scan ./patches and DDE3_PATCH_DIR")
                .clicked()
            {
                self.reload_patches();
            }
            if ui
                .add_enabled(
                    self.image.is_some(),
                    egui::Button::new("Save image…").min_size(Vec2::new(140.0, 30.0)),
                )
                .on_disabled_hover_text("Load an image first")
                .on_hover_text("Save the patched image to disk (Ctrl+S)")
                .clicked()
            {
                self.save_image();
            }
            ui.label(
                RichText::new(format!("{} manifests", self.patches.len()))
                    .size(11.0)
                    .color(MUTED),
            );
        });
        if !self.patch_warnings.is_empty() {
            ui.add_space(8.0);
            card(ui, ui.available_width(), |ui| {
                section_title(ui, "Manifest warnings");
                for w in &self.patch_warnings {
                    ui.label(RichText::new(w).size(11.0).color(WARN));
                }
            });
        }
        ui.add_space(12.0);
        if self.image.is_none() {
            card(ui, ui.available_width(), |ui| {
                section_title(ui, "No image loaded");
                ui.label(
                    RichText::new(
                        "Open a 256 KB full read to see each manifest's state and to apply or \
                         revert a patch. Nothing is written to the ECU from this tab.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                if centered_button(
                    ui,
                    Vec2::new(150.0, 30.0),
                    egui::Button::new("Open image…"),
                )
                .clicked()
                {
                    self.open_image_dialog();
                }
            });
            ui.add_space(10.0);
        }
        if self.patches.is_empty() {
            card(ui, ui.available_width(), |ui| {
                section_title(ui, "No patches");
                ui.label(
                    RichText::new(
                        "Put a patch JSON manifest in ./patches or set DDE3_PATCH_DIR, or use \
                         Open patch….",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
            });
            return;
        }

        let image = if self.image_is_partial {
            None
        } else {
            self.image.clone()
        };
        let mut selected = None;
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Available patches");
            ScrollArea::vertical().max_height(190.0).show(ui, |ui| {
                for (i, p) in self.patches.iter().enumerate() {
                    let state = self.patch_state(p);
                    let text = format!("{}   ·   {}", p.name, state.label());
                    if ui
                        .selectable_label(i == self.patch_selected, RichText::new(text).size(13.0))
                        .clicked()
                    {
                        selected = Some(i);
                    }
                    if self.image.is_some() {
                        if let Some(detail) = state.detail() {
                            ui.label(RichText::new(detail).size(10.0).color(MUTED));
                        }
                    }
                    ui.add_space(2.0);
                }
            });
        });
        if let Some(i) = selected {
            self.patch_selected = i;
            self.patch_notice = None;
            self.patch_target_ok = image.as_ref().map(|img| self.patches[i].reference_matches(img));
        }

        let Some(p) = self.patches.get(self.patch_selected).cloned() else {
            return;
        };
        if self.patch_target_ok.is_none() {
            if let Some(img) = &image {
                self.patch_target_ok = Some(p.reference_matches(img));
            }
        }
        let state = self.patch_state(&p);
        let can_apply = state == PatchState::Available;
        let can_revert = state == PatchState::Applied;

        ui.add_space(12.0);
        card(ui, ui.available_width(), |ui| {
            section_title(ui, &p.name);
            ui.horizontal_wrapped(|ui| {
                pill(ui, &format!("v{}", p.version), ACCENT);
                pill(
                    ui,
                    state.label(),
                    if can_apply {
                        WARN
                    } else if can_revert {
                        OK
                    } else {
                        DANGER
                    },
                );
                if p.hardware_tested {
                    pill(ui, "hardware tested", OK);
                } else {
                    pill(ui, "not hardware tested", WARN);
                }
                if p.checksum == "auto" || p.checksum == "already-corrected" {
                    pill(ui, "checksums corrected on apply", ACCENT);
                }
            });
            ui.add_space(6.0);
            if !p.description.is_empty() {
                ui.label(RichText::new(&p.description).size(12.0).color(TEXT));
            }
            if !p.verified.is_empty() {
                ui.label(RichText::new(&p.verified).size(11.0).color(MUTED));
            }
            ui.add_space(6.0);
            egui::Grid::new("patch-meta")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Source").size(11.0).color(MUTED));
                    ui.label(RichText::new(p.source.label()).size(11.0).color(TEXT));
                    ui.end_row();
                    if let Some(out) = &p.output {
                        ui.label(RichText::new("Builder output").size(11.0).color(MUTED));
                        ui.label(RichText::new(out).size(11.0).color(TEXT));
                        ui.end_row();
                    }
                    if let Some(hash) = &p.target_sha256 {
                        ui.label(RichText::new("Reference read").size(11.0).color(MUTED));
                        let text = if self.patch_target_ok == Some(true) {
                            format!("{} (the read this manifest was built from)", &hash[..hash.len().min(16)])
                        } else if self.patch_target_ok == Some(false) {
                            format!("{} (informational — any read with matching edits patches fine)", &hash[..hash.len().min(16)])
                        } else {
                            hash[..hash.len().min(16)].to_string()
                        };
                        ui.label(
                            RichText::new(text)
                                .font(FontId::monospace(10.0))
                                .color(MUTED),
                        );
                        ui.end_row();
                    }
                    ui.label(RichText::new("Content").size(11.0).color(MUTED));
                    if let Some(img) = &image {
                        let (matched, total) = p.edit_match(img);
                        ui.label(
                            RichText::new(format!("{matched}/{total} edits match the loaded image"))
                                .size(11.0)
                                .color(if matched == total { OK } else { WARN }),
                        );
                    } else {
                        ui.label(RichText::new("load a 256 KB image to compare").size(11.0).color(MUTED));
                    }
                    ui.end_row();
                    if !p.sectors.is_empty() {
                        ui.label(RichText::new("Sectors").size(11.0).color(MUTED));
                        ui.label(
                            RichText::new(
                                p.sectors
                                    .iter()
                                    .map(|s| format!("{s}"))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )
                            .size(11.0)
                            .color(TEXT),
                        );
                        ui.end_row();
                    }
                    ui.label(RichText::new("Changes").size(11.0).color(MUTED));
                    ui.label(
                        RichText::new(format!(
                            "{} ranges, {} bytes",
                            p.changes.len(),
                            p.changed_bytes()
                        ))
                        .size(11.0)
                        .color(TEXT),
                    );
                    ui.end_row();
                });
            if !p.checksum_note.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&p.checksum_note).size(11.0).color(MUTED));
            }
            if let Some(detail) = state.detail() {
                ui.add_space(6.0);
                ui.label(RichText::new(detail).size(11.0).color(DANGER));
            }
            if let Some(note) = &self.patch_notice {
                ui.add_space(6.0);
                ui.label(RichText::new(note).size(12.0).color(OK));
            }
        });

        ui.add_space(12.0);
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Changed ranges");
            egui::Grid::new("patch-changes")
                .num_columns(6)
                .spacing([12.0, 5.0])
                .striped(true)
                .show(ui, |ui| {
                    for h in ["What", "Offset", "Len", "Before", "After", "State"] {
                        ui.label(RichText::new(h).size(11.0).color(MUTED));
                    }
                    ui.end_row();
                    for c in &p.changes {
                        let (label, color) = match &image {
                            Some(img) if img[c.offset..c.offset + c.after.len()] == c.after => {
                                ("applied", OK)
                            }
                            Some(img) if img[c.offset..c.offset + c.before.len()] == c.before => {
                                ("original", MUTED)
                            }
                            _ => ("conflict", DANGER),
                        };
                        ui.label(RichText::new(&c.what).size(11.0).color(TEXT));
                        ui.label(
                            RichText::new(format!("0x{:05X}", c.offset))
                                .font(FontId::monospace(11.0))
                                .color(TEXT),
                        );
                        ui.label(RichText::new(format!("{}", c.after.len())).size(11.0).color(MUTED));
                        ui.label(
                            RichText::new(short_hex(&c.before, 8))
                                .font(FontId::monospace(10.0))
                                .color(MUTED),
                        );
                        ui.label(
                            RichText::new(short_hex(&c.after, 8))
                                .font(FontId::monospace(10.0))
                                .color(ACCENT),
                        );
                        ui.label(RichText::new(label).size(11.0).color(color));
                        ui.end_row();
                    }
                });
        });

        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    can_apply && self.image.is_some(),
                    egui::Button::new(RichText::new("Apply to image").color(BG))
                        .fill(WARN)
                        .min_size(Vec2::new(170.0, 34.0)),
                )
                .on_disabled_hover_text(if self.image.is_none() {
                    "Load a 256 KB full image first"
                } else if can_revert {
                    "Already applied — use Revert to remove it"
                } else {
                    "The image does not match this manifest's before-bytes"
                })
                .on_hover_text("Write the after-bytes and correct every checksum")
                .clicked()
            {
                self.apply_selected_patch();
            }
            if ui
                .add_enabled(
                    can_revert && self.image.is_some(),
                    egui::Button::new(RichText::new("Revert in image").color(BG))
                        .fill(ACCENT)
                        .min_size(Vec2::new(170.0, 34.0)),
                )
                .on_disabled_hover_text("Only an applied patch can be reverted")
                .on_hover_text("Write the before-bytes back and correct every checksum")
                .clicked()
            {
                self.revert_selected_patch();
            }
            if ui
                .add_enabled(
                    can_revert && !self.checks_ok(),
                    egui::Button::new("Correct checksums")
                        .min_size(Vec2::new(170.0, 34.0)),
                )
                .on_disabled_hover_text(
                    "Checksums are already valid (or no patch is applied)",
                )
                .on_hover_text("Recompute TDI slots + program chain without changing any edit")
                .clicked()
            {
                self.correct_image_checksums();
            }
            if ui
                .add_sized(Vec2::new(150.0, 34.0), egui::Button::new("Go to Write"))
                .clicked()
            {
                self.tab = Tab::Write;
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Apply writes the after-bytes and corrects all flash checksums (TDI code + \
                 calibration, then the cold-start program chain). Revert writes the before-bytes. \
                 The original image must match every before-range; a mismatch is refused.",
            )
            .size(11.0)
            .color(MUTED),
        );
    }

    fn apply_selected_patch(&mut self) {
        let Some(p) = self.patches.get(self.patch_selected).cloned() else {
            return;
        };
        let Some(mut img) = self.image.take() else {
            self.error = Some("Load a 256 KB image first".into());
            return;
        };
        match p.apply(&mut img) {
            Ok(()) => {
                self.image = Some(img);
                self.refresh_checks();
                self.patch_target_ok = self.image.as_ref().map(|i| p.reference_matches(i));
                self.patch_notice = Some(format!(
                    "Applied {}; checksums corrected. Save the image, then write it.",
                    p.name
                ));
                self.status = format!("Applied patch {}", p.name);
                self.error = None;
                self.push_log(format!("PATCH apply {}", p.name));
            }
            Err(e) => {
                self.image = Some(img);
                self.error = Some(format!("{e}"));
            }
        }
    }

    fn revert_selected_patch(&mut self) {
        let Some(p) = self.patches.get(self.patch_selected).cloned() else {
            return;
        };
        let Some(mut img) = self.image.take() else {
            self.error = Some("Load a 256 KB image first".into());
            return;
        };
        match p.revert(&mut img) {
            Ok(()) => {
                self.image = Some(img);
                self.refresh_checks();
                self.patch_target_ok = self.image.as_ref().map(|i| p.reference_matches(i));
                self.patch_notice = Some(format!(
                    "Reverted {}; checksums corrected. Save the image, then write it.",
                    p.name
                ));
                self.status = format!("Reverted patch {}", p.name);
                self.error = None;
                self.push_log(format!("PATCH revert {}", p.name));
            }
            Err(e) => {
                self.image = Some(img);
                self.error = Some(format!("{e}"));
            }
        }
    }

    /// Recompute every checksum (TDI slots + program chain) on the loaded
    /// image. This is the repair path for an image that is already patched but
    /// carries a stale/bypassed program checksum (e.g. a third-party EWS-off
    /// read) — no `before` bytes are needed.
    fn correct_image_checksums(&mut self) {
        if self.image_is_partial {
            self.error = Some("Checksum correction needs a full 256 KB image".into());
            return;
        }
        let Some(img) = self.image.take() else {
            return;
        };
        let fixed = checksum::correct(&img);
        let changed = fixed != img;
        self.image = Some(fixed);
        self.refresh_checks();
        self.patch_target_ok = self.image.as_ref().and_then(|i| {
            self.patches
                .get(self.patch_selected)
                .map(|p| p.reference_matches(i))
        });
        self.status = if changed {
            "Checksums corrected".into()
        } else {
            "Checksums already valid".into()
        };
        self.error = None;
        self.push_log(format!("PATCH checksums corrected (changed={changed})"));
    }

    fn open_patch_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Patch manifest", &["json"])
            .pick_file()
        else {
            return;
        };
        match patches::load_file(&path) {
            Ok(p) => {
                let name = p.name.clone();
                if let Some(existing) = self.patches.iter_mut().find(|x| x.name == p.name) {
                    *existing = p;
                } else {
                    self.patches.push(p);
                }
                self.patches.sort_by(|a, b| a.name.cmp(&b.name));
                if let Some(i) = self.patches.iter().position(|x| x.name == name) {
                    self.patch_selected = i;
                }
                self.patch_target_ok = None;
                self.patch_notice = Some(format!("Loaded manifest {}", path.display()));
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
    }

    fn reload_patches(&mut self) {
        let (patches, warnings) = patches::load_all();
        self.patches = patches;
        self.patch_warnings = warnings;
        self.patch_selected = self
            .patch_selected
            .min(self.patches.len().saturating_sub(1));
        self.patch_target_ok = None;
        self.patch_notice = Some("Patch manifests reloaded".into());
    }

    fn checksum_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Checksums", "Bosch TDI V3.1 block checksums + the cold-start program chain — EDC15M is the DDE3 (same ECU). Algorithms ported from the verified edc15m-checksum-tool and progsum.py.");
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if centered_button_enabled(
                ui,
                self.image.is_some() && !self.busy,
                Vec2::new(140.0, 30.0),
                egui::Button::new("Correct all"),
            )
            .clicked()
            {
                self.correct_all_checksums();
            }
            if centered_button_enabled(
                ui,
                self.image.is_some() && !self.busy,
                Vec2::new(140.0, 30.0),
                egui::Button::new("Save image…"),
            )
            .clicked()
            {
                self.save_image();
            }
            if centered_button_enabled(
                ui,
                !self.busy,
                Vec2::new(140.0, 30.0),
                egui::Button::new("Open image…"),
            )
            .on_hover_text("Open a 256 KB full or 32 KB calibration .bin (Ctrl+O)")
            .clicked()
            {
                self.open_image_dialog();
            }
            if self.image.is_some()
                && centered_button_enabled(
                    ui,
                    !self.busy,
                    Vec2::new(120.0, 30.0),
                    egui::Button::new("Close image"),
                )
                .on_disabled_hover_text("An operation is running")
                .on_hover_text("Unload the image from the toolkit")
                .clicked()
            {
                self.unload_image();
            }
            ui.label(
                RichText::new("TDI V3.1 · 2-byte block id at 0xB7A")
                    .size(11.0)
                    .color(MUTED),
            );
        });
        ui.add_space(12.0);
        if self.image.is_none() {
            card(ui, ui.available_width(), |ui| {
                section_title(ui, "No image");
                ui.label(
                    RichText::new("Load a 256 KB full image or a 32 KB calibration image.")
                        .color(MUTED),
                );
                ui.add_space(6.0);
                if centered_button(
                    ui,
                    Vec2::new(150.0, 30.0),
                    egui::Button::new("Open image…"),
                )
                .clicked()
                {
                    self.open_image_dialog();
                }
            });
            return;
        }
        let mut apply_idx = None;
        let mut fix_idx = None;
        let none = self.checks.is_empty();
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Checksums");
            if none {
                ui.label(RichText::new("No EDC15M TDI V3.1 checksum block found in this image. The block is located by its 2-byte id at 0xB7A; a blank or foreign image has none.").color(MUTED));
            } else {
                egui::Grid::new("checksum-blocks")
                    .num_columns(8)
                    .spacing([14.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in [
                            "Block",
                            "Pos",
                            "Stored",
                            "Computed",
                            "Status",
                            "Stored (edit)",
                            "Apply",
                            "Fix",
                        ] {
                            ui.label(RichText::new(h).size(11.0).color(MUTED));
                        }
                        ui.end_row();
                        for (i, c) in self.checks.iter().enumerate() {
                            let pos = self.file_offset_of(c.pos);
                            ui.label(RichText::new(c.name).size(12.0).color(TEXT));
                            ui.label(
                                RichText::new(format!("0x{pos:05X}"))
                                    .font(FontId::monospace(11.0))
                                    .color(MUTED),
                            );
                            ui.label(
                                RichText::new(format!("{:#010X}", c.stored))
                                    .font(FontId::monospace(11.0))
                                    .color(MUTED),
                            );
                            ui.label(
                                RichText::new(format!("{:#010X}", c.computed))
                                    .font(FontId::monospace(11.0))
                                    .color(MUTED),
                            );
                            ui.label(
                                RichText::new(if c.ok { "OK" } else { "BAD" })
                                    .size(11.0)
                                    .color(if c.ok { OK } else { DANGER }),
                            );
                            if i < self.comp_edit.len() {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.comp_edit[i])
                                        .desired_width(92.0),
                                );
                            } else {
                                ui.label("");
                            }
                            if ui.small_button("Apply").clicked() {
                                apply_idx = Some(i);
                            }
                            if centered_button_enabled(
                                ui,
                                true,
                                Vec2::new(48.0, 22.0),
                                egui::Button::new("Fix"),
                            )
                            .clicked()
                            {
                                fix_idx = Some(i);
                            }
                            ui.end_row();
                        }
                    });
            }
        });
        if let Some(i) = apply_idx {
            self.apply_comp_edit(i);
        }
        if let Some(i) = fix_idx {
            self.correct_block_checksum(i);
        }
        ui.add_space(10.0);
        ui.label(RichText::new(if self.image_is_partial {
            "A 32 KB calibration image carries one checksum at file offset 0x7BFC. The code block (0x13FFC) only exists in 256 KB reads."
        } else {
            "Pos is the file offset of the 4-byte checksum. Stored (edit) writes the typed word into the image; Fix recomputes one block; Correct all fixes every discovered block."
        }).size(11.0).color(MUTED));
        ui.add_space(10.0);
        ui.label(RichText::new("⚠️ The TDI V3.1 algorithm is verified against edc15m-checksum-tool (regression vectors + real E46 reads). The flash-write path is still experimental — keep a verified backup.").size(10.0).color(WARN));
    }

    fn dtc_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Fault codes",
            "DS2 fault memory — FS_LESEN / FS_SHADOW_LESEN / FS_LOESCHEN (DDE30DS0.prg).",
        );
        ui.add_space(8.0);
        card(ui, ui.available_width(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if centered_button_enabled(
                    ui,
                    self.connected && !self.busy,
                    Vec2::new(150.0, 34.0),
                    egui::Button::new(RichText::new("Read faults").color(BG)).fill(ACCENT),
                )
                .clicked()
                {
                    self.start_dtc_read();
                }
                if self.dtc_clear_armed {
                    if centered_button_enabled(
                        ui,
                        self.connected && !self.busy,
                        Vec2::new(150.0, 34.0),
                        egui::Button::new(RichText::new("Confirm clear").color(BG)).fill(DANGER),
                    )
                    .clicked()
                    {
                        self.start_dtc_clear();
                    }
                    if ui.small_button("cancel").clicked() {
                        self.dtc_clear_armed = false;
                    }
                    ui.label(
                        RichText::new("next click erases the fault memory")
                            .size(11.0)
                            .color(DANGER),
                    );
                } else if centered_button_enabled(
                    ui,
                    self.connected && !self.busy && !self.dtcs.is_empty(),
                    Vec2::new(150.0, 34.0),
                    egui::Button::new("Clear faults").fill(CARD_ALT),
                )
                .clicked()
                {
                    self.dtc_clear_armed = true;
                }
                if centered_button_enabled(
                    ui,
                    self.connected && !self.busy,
                    Vec2::new(120.0, 34.0),
                    egui::Button::new("Battery ⚡"),
                )
                .clicked()
                {
                    self.start_battery();
                }
                if let Some(v) = self.battery_v {
                    ui.label(
                        RichText::new(format!("Battery {v:.2} V"))
                            .size(13.0)
                            .strong()
                            .color(if v < 10.5 { WARN } else { OK }),
                    );
                }
            });
            if self.active_job == Some(Job::Dtc) || self.active_job == Some(Job::DtcClear) {
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .animate(true),
                );
                ui.label(RichText::new(&self.progress_text).size(11.0).color(MUTED));
            }
        });
        ui.add_space(14.0);
        if self.dtcs.is_empty() {
            card(ui, ui.available_width(), |ui| {
                section_title(ui, "Fault memory");
                ui.label(
                    RichText::new(if self.active_job == Some(Job::Dtc) {
                        "Reading…"
                    } else {
                        "No fault codes stored yet — press Read faults."
                    })
                    .color(MUTED),
                );
            });
            return;
        }
        for (i, d) in self.dtcs.iter().enumerate() {
            card(ui, ui.available_width(), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("#{}", i + 1))
                            .size(13.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.label(
                        RichText::new(format!(
                            "ORT 0x{:02X}  {}",
                            d.ort,
                            crate::dtc::ort_text(d.ort)
                        ))
                        .size(14.0)
                        .strong()
                        .color(TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "mask 0x{:02X} · freq {}",
                                d.art_mask, d.freq
                            ))
                            .font(FontId::monospace(11.0))
                            .color(MUTED),
                        );
                    });
                });
                ui.add_space(6.0);
                let legacy = [d.art];
                let codes: &[u8] = if d.arts.is_empty() { &legacy } else { &d.arts };
                for a in codes {
                    ui.label(
                        RichText::new(format!("Fault: {}", crate::dtc::fart_text(*a)))
                            .size(13.0)
                            .color(WARN),
                    );
                }
                if !d.uw.is_empty() {
                    ui.add_space(6.0);
                    let hex: String = d.uw.iter().map(|v| format!("{v:04X} ")).collect();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("freeze (BE u16)").size(12.0).color(MUTED));
                        ui.label(
                            RichText::new(hex.trim_end().to_string())
                                .font(FontId::monospace(12.0))
                                .color(ACCENT),
                        );
                    });
                }
                if !d.record.is_empty() {
                    egui::CollapsingHeader::new(
                        RichText::new(format!("Raw record ({} bytes)", d.record.len()))
                            .size(12.0)
                            .color(MUTED),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        let hex: String = d
                            .record
                            .iter()
                            .map(|b| format!("{:02X} ", b))
                            .collect();
                        ui.label(
                            RichText::new(hex.trim_end())
                                .font(FontId::monospace(11.0))
                                .color(MUTED),
                        );
                    });
                }
                if !d.shadow.is_empty() {
                    egui::CollapsingHeader::new(
                        RichText::new(format!("Freeze frame ({} values)", d.shadow.len()))
                            .size(12.0)
                            .color(MUTED),
                    )
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::Grid::new(format!("dtc-fs-{}", i))
                            .num_columns(3)
                            .spacing([16.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Measurement").size(11.0).color(MUTED));
                                ui.label(RichText::new("Value").size(11.0).color(MUTED));
                                ui.label(RichText::new("Raw").size(11.0).color(MUTED));
                                ui.end_row();
                                for (j, raw) in d.shadow.iter().enumerate() {
                                    let fallback = format!("[{j}]");
                                    let (id, name, unit) = match crate::dtc::UMWELT.get(j) {
                                        Some(r) if r.0 != 0x00 && r.0 != 0xFF => {
                                            (r.0, r.1.trim(), r.2.trim())
                                        }
                                        _ => (0, fallback.as_str(), ""),
                                    };
                                    ui.label(RichText::new(name).size(12.0).color(MUTED));
                                    if id != 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "{:.2} {}",
                                                crate::dtc::uw_value(id, *raw),
                                                unit
                                            ))
                                            .font(FontId::monospace(12.0))
                                            .color(TEXT),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new(format!("{raw}"))
                                                .font(FontId::monospace(12.0))
                                                .color(MUTED),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(format!("{raw}"))
                                            .font(FontId::monospace(12.0))
                                            .color(MUTED),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
                }
            });
            ui.add_space(10.0);
        }
    }

    fn var_picker_window(&mut self, ctx: &egui::Context) {
        if !self.show_var_picker {
            return;
        }
        let mut open = self.show_var_picker;
        let mut changed = false;
        egui::Window::new("Live variables")
            .open(&mut open)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("All").clicked() {
                        for v in self.live_enabled.iter_mut() {
                            *v = true;
                        }
                        changed = true;
                    }
                    if ui.small_button("None").clicked() {
                        for v in self.live_enabled.iter_mut() {
                            *v = false;
                        }
                        changed = true;
                    }
                    ui.label(
                        RichText::new("max 10 IDs per request")
                            .size(11.0)
                            .color(MUTED),
                    );
                });
                ui.add_space(6.0);
                let mut groups: Vec<&'static str> = Vec::new();
                for v in LIVE_VARS {
                    if !groups.contains(&v.group) {
                        groups.push(v.group);
                    }
                }
                ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
                    for g in groups {
                        egui::CollapsingHeader::new(g)
                            .default_open(false)
                            .show(ui, |ui| {
                                for (i, v) in LIVE_VARS.iter().enumerate() {
                                    if v.group == g
                                        && ui.checkbox(&mut self.live_enabled[i], v.name).changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                    }
                });
            });
        self.show_var_picker = open;
        if changed {
            if self.logging {
                self.status = "Variable changes apply after logging stops".into();
            } else if self.live_running {
                if self.live_enabled.iter().any(|v| *v) {
                    self.stop_live();
                    self.start_live();
                } else {
                    self.stop_live();
                }
            }
        }
    }

    fn live_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Live data",
            "Polled via MW_SELECT_LESEN (`12 05 0B 10 <id bytes>`) — service 0x0B.",
        );
        ui.add_space(8.0);
        let enabled_count = self.live_enabled.iter().filter(|v| **v).count();
        ui.horizontal_wrapped(|ui| {
            if self.live_running {
                if centered_button(
                    ui,
                    Vec2::new(130.0, 30.0),
                    egui::Button::new(RichText::new("Stop").color(DANGER)).fill(CARD_ALT),
                )
                .clicked()
                {
                    self.stop_live();
                }
            } else if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(130.0, 30.0),
                egui::Button::new(RichText::new("Start").color(BG)).fill(ACCENT),
            )
            .clicked()
            {
                self.start_live();
            }
            if centered_button(
                ui,
                Vec2::new(130.0, 30.0),
                egui::Button::new(format!("Variables ({enabled_count})")),
            )
            .clicked()
            {
                self.show_var_picker = !self.show_var_picker;
            }
            if centered_button(
                ui,
                Vec2::new(130.0, 30.0),
                egui::Button::new("Graph").selected(self.show_graph),
            )
            .clicked()
            {
                self.show_graph = !self.show_graph;
            }
            // The link-speed group (faster 9600, fast baud 125k) moved to
            // the side-nav LINK section — it is program-wide, not a Live
            // tab concern.
            ui.label(
                RichText::new(if self.live_running {
                    format!("polling at {:.1} Hz", self.live_rate)
                } else {
                    "idle".to_string()
                })
                .size(12.0)
                .color(MUTED),
            );
            if enabled_count > 10 {
                ui.label(
                    RichText::new("more than 10 selected: polled in groups")
                        .size(11.0)
                        .color(WARN),
                );
            }
        });
        ui.add_space(12.0);
        if self.show_graph {
            graph_card(
                ui,
                &self.live_enabled,
                &self.live_values,
                &self.live_history,
                &mut self.graph_var,
            );
            ui.add_space(12.0);
        }
        let active: Vec<usize> = (0..LIVE_VARS.len())
            .filter(|i| self.live_enabled[*i])
            .collect();
        if active.is_empty() {
            ui.label(RichText::new("No variables selected.").color(MUTED));
            return;
        }
        let avail = ui.available_width();
        let cols = if avail > 1250.0 {
            3
        } else if avail > 820.0 {
            2
        } else {
            1
        };
        let width = (avail - 12.0 * (cols - 1) as f32) / cols as f32;
        egui::Grid::new("live-grid")
            .num_columns(cols)
            .spacing([12.0, 12.0])
            .show(ui, |ui| {
                for (n, i) in active.iter().enumerate() {
                    live_card(
                        ui,
                        &LIVE_VARS[*i],
                        self.live_values[*i],
                        &self.live_history[*i],
                        width,
                    );
                    if (n + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });
    }

    fn logger_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Value logger",
            "Poll the selected measurement IDs and write a timestamped CSV.",
        );
        ui.add_space(8.0);
        let enabled: Vec<usize> = (0..LIVE_VARS.len())
            .filter(|i| self.live_enabled[*i])
            .collect();
        ui.horizontal_wrapped(|ui| {
            if self.logging {
                if centered_button(
                    ui,
                    Vec2::new(160.0, 34.0),
                    egui::Button::new(RichText::new("Stop logging").color(DANGER)).fill(CARD_ALT),
                )
                .clicked()
                {
                    self.stop_logging();
                }
            } else if centered_button_enabled(
                ui,
                self.connected && !self.busy,
                Vec2::new(160.0, 34.0),
                egui::Button::new(RichText::new("Start logging").color(BG)).fill(ACCENT),
            )
            .clicked()
            {
                self.start_logging();
            }
            if centered_button(
                ui,
                Vec2::new(160.0, 34.0),
                egui::Button::new(format!("Variables ({})", enabled.len())),
            )
            .clicked()
            {
                self.show_var_picker = !self.show_var_picker;
            }
            ui.label(
                RichText::new(format!("{:.1} Hz · {} rows", self.live_rate, self.log_rows))
                    .size(12.0)
                    .color(MUTED),
            );
        });
        ui.add_space(6.0);
        if !self.log_path.is_empty() {
            ui.label(
                RichText::new(format!("File: {}", self.log_path))
                    .size(11.0)
                    .color(MUTED),
            );
        }
        if self.logging {
            ui.label(
                RichText::new("Logging is active. Stop to flush and close the CSV.")
                    .size(11.0)
                    .color(WARN),
            );
        }
        ui.add_space(12.0);
        card(ui, ui.available_width(), |ui| {
            section_title(ui, "Current values");
            if enabled.is_empty() {
                ui.label(RichText::new("No variables selected.").color(MUTED));
                return;
            }
            ScrollArea::vertical().max_height(430.0).show(ui, |ui| {
                egui::Grid::new("logger-values")
                    .num_columns(3)
                    .striped(true)
                    .spacing([18.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Variable").size(11.0).color(MUTED));
                        ui.label(RichText::new("Value").size(11.0).color(MUTED));
                        ui.label(RichText::new("Unit").size(11.0).color(MUTED));
                        ui.end_row();
                        for &i in &enabled {
                            ui.label(RichText::new(LIVE_VARS[i].name).size(12.0).color(TEXT));
                            ui.label(
                                RichText::new(format!("{:.2}", self.live_values[i]))
                                    .font(FontId::monospace(12.0))
                                    .color(ACCENT),
                            );
                            ui.label(RichText::new(LIVE_VARS[i].unit).size(11.0).color(MUTED));
                            ui.end_row();
                        }
                    });
            });
        });
    }

    fn log_tab(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Protocol log",
            "Raw TX/RX telegrams and operation messages.",
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if centered_button(ui, Vec2::new(80.0, 28.0), egui::Button::new("Copy"))
                .on_hover_text("Copy the whole log to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(self.logs.join("\n"));
            }
            if centered_button(ui, Vec2::new(80.0, 28.0), egui::Button::new("Clear"))
                .on_hover_text("Clear the protocol log (the run-log file keeps everything)")
                .clicked()
            {
                self.logs.clear();
            }
            ui.label(
                RichText::new(format!("{} lines", self.logs.len()))
                    .size(12.0)
                    .color(MUTED),
            );
            if !self.run_log_path.as_os_str().is_empty() {
                ui.label(
                    RichText::new(format!("→ {}", self.run_log_path.display()))
                        .size(11.0)
                        .monospace()
                        .color(MUTED),
                )
                .on_hover_text("Full run log — every line of this run, untruncated");
            }
        });
        ui.add_space(8.0);
        let log_height = (ui.available_height() - 12.0).max(240.0);
        Frame::new()
            .fill(Color32::from_rgb(8, 12, 15))
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(10)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_min_height(log_height);
                ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(log_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in &self.logs {
                            ui.label(RichText::new(line).font(FontId::monospace(11.0)).color(
                                if line.starts_with("ERROR") {
                                    DANGER
                                } else if line.starts_with("TX") {
                                    ACCENT
                                } else {
                                    TEXT
                                },
                            ));
                        }
                    });
            });
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(f) = self.run_log.as_mut() {
            let (_, stamp) = now_local();
            let _ = writeln!(f, "{stamp} run ended");
            let _ = f.flush();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        // Ctrl+O open image, Ctrl+S save image. Skipped while a text field has
        // focus so typing is never hijacked.
        if !ctx.wants_keyboard_input() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
                self.open_image_dialog();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::S)) {
                self.save_image();
            }
        }
        if self.busy || self.live_running {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.top_bar(ctx);
        self.side_nav(ctx);
        self.central(ctx);
        self.post_write_modal(ctx);
    }
}

impl App {
    /// Shown once after a successful write: the ECU must be power-cycled
    /// before its (rebuilt) fault memory can be cleared.
    fn post_write_modal(&mut self, ctx: &egui::Context) {
        if !self.post_write_pending {
            return;
        }
        let mut confirm = false;
        let mut skip = false;
        egui::Modal::new(egui::Id::new("post_write_clear")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Power cycle the ECU");
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "The write is complete. Switch the ignition/ECU power off and on, wait for the ECU to boot, then press Confirm. The tool waits 5 s and clears the fault memory.",
                )
                .size(12.0)
                .color(TEXT),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if centered_button(ui, Vec2::new(190.0, 32.0), egui::Button::new(RichText::new("Confirm power cycle").color(BG)).fill(ACCENT)).clicked() {
                    confirm = true;
                }
                if centered_button(ui, Vec2::new(120.0, 32.0), egui::Button::new("Skip")).clicked() {
                    skip = true;
                }
            });
        });
        if confirm {
            self.post_write_pending = false;
            self.start_post_write_clear();
        } else if skip {
            self.post_write_pending = false;
            self.status = "Write complete (fault memory not cleared)".into();
        }
    }
}

// ---------------------------------------------------------------------------
// Styling and helpers
// ---------------------------------------------------------------------------

/// Embedded so the UI renders identically on Windows and Linux. The egui default
/// fonts have no glyphs for the sidebar icons, the status dot or the arrow, which
/// showed as tofu boxes on systems without a matching fallback font.
const UI_FONT: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu".to_owned(),
        Arc::new(egui::FontData::from_static(UI_FONT)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("dejavu".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Elapsed job time for the progress-bar timer: `M:SS`, or `H:MM:SS`
/// past one hour.
fn format_job_time(d: Duration) -> String {
    let s = d.as_secs();
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// Progress bar with the elapsed job time overlaid on its right end.
/// egui draws the percentage at the LEFT of the bar, so the right end is
/// free; the near-white monospace timer carries a soft dark shadow so it
/// stays readable over both the dark track and the teal fill.
fn progress_with_timer(ui: &mut Ui, progress: f32, started: Option<Instant>) {
    let response = ui.add(
        egui::ProgressBar::new(progress)
            .show_percentage()
            .animate(true),
    );
    if let Some(t0) = started {
        let rect = response.rect;
        let text = format_job_time(t0.elapsed());
        let font = FontId::monospace(11.5);
        let pos = pos2(rect.right() - 10.0, rect.center().y);
        let shadow = Color32::from_black_alpha(160);
        ui.painter()
            .text(pos + vec2(1.0, 1.0), Align2::RIGHT_CENTER, &text, font.clone(), shadow);
        ui.painter().text(
            pos,
            Align2::RIGHT_CENTER,
            &text,
            font,
            Color32::from_rgb(240, 248, 245),
        );
    }
}

fn centered_button(ui: &mut Ui, size: Vec2, button: egui::Button<'_>) -> egui::Response {
    let layout = Layout::left_to_right(Align::Center).with_main_align(Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| ui.add_sized(size, button))
        .inner
}

fn centered_button_enabled(
    ui: &mut Ui,
    enabled: bool,
    size: Vec2,
    button: egui::Button<'_>,
) -> egui::Response {
    let layout = Layout::left_to_right(Align::Center).with_main_align(Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| {
        ui.add_enabled_ui(enabled, |ui| ui.add_sized(size, button))
            .inner
    })
    .inner
}

fn style(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.extreme_bg_color = Color32::from_rgb(7, 10, 13);
    style.visuals.faint_bg_color = CARD_ALT;
    style.visuals.widgets.noninteractive.bg_fill = CARD;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.inactive.bg_fill = CARD_ALT;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(33, 44, 54);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_DIM);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(38, 52, 63);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    style.visuals.selection.bg_fill = ACCENT_DIM;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    style.visuals.hyperlink_color = ACCENT;
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(24.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(11.0));
    ctx.set_style(style);
}

fn heading(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(
        RichText::new(title)
            .font(FontId::proportional(26.0))
            .strong()
            .color(TEXT),
    );
    ui.label(RichText::new(subtitle).size(12.0).color(MUTED));
}

fn card(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    let inner = (width - 32.0).max(0.0);
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(inner);
            add(ui);
        });
}

/// Shorten a path/name from the left, keeping the tail visible.
fn short_name(name: &str, max_chars: usize) -> String {
    let n = name.chars().count();
    if n <= max_chars {
        return name.to_string();
    }
    let tail: String = name.chars().skip(n - max_chars.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// Why the write buttons are disabled — shown on hover so the user is never
/// left guessing which precondition is missing.
fn write_disabled_reason(
    has_image: bool,
    partial: bool,
    connected: bool,
    busy: bool,
    ready: bool,
) -> &'static str {
    if busy {
        "An operation is running"
    } else if !connected {
        "Connect to the ECU first"
    } else if !has_image {
        "Open an image first"
    } else if partial {
        "Calibration image loaded — use Write calibration only"
    } else if !ready {
        "Confirm the backup and type WRITE first"
    } else {
        "Not ready"
    }
}

/// First `max` bytes as hex, with an ellipsis when truncated.
fn short_hex(bytes: &[u8], max: usize) -> String {
    let mut s: String = bytes
        .iter()
        .take(max)
        .map(|b| format!("{b:02x}"))
        .collect();
    if bytes.len() > max {
        s.push('…');
    }
    s
}

fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).strong().color(TEXT));
    ui.add_space(8.0);
}

/// Battery status pill (DDE3 ADF 0x0F65 scale 0.02037243 / LSB).
/// Shown in the top bar (global): a weak battery mid-flash is the classic
/// brick cause, worth the glance.
fn battery_pill(ui: &mut Ui, voltage: Option<f64>) {
    match voltage {
        Some(v) if v < 11.5 => pill(ui, &format!("Battery {v:.1} V — critical"), DANGER),
        Some(v) if v < 12.0 => pill(ui, &format!("Battery {v:.1} V — low"), WARN),
        Some(v) => pill(ui, &format!("Battery {v:.1} V"), OK),
        None => pill(ui, "Battery not measured", MUTED),
    }
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            22,
        ))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(20)
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(color));
        });
}

fn checksum_card(ui: &mut Ui, checks: &[checksum::Checksum]) {
    card(ui, ui.available_width(), |ui| {
        section_title(ui, "Flash checksum");
        egui::Grid::new("checks")
            .num_columns(3)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Block").size(11.0).color(MUTED));
                ui.label(RichText::new("Checksum").size(11.0).color(MUTED));
                ui.label(RichText::new("Status").size(11.0).color(MUTED));
                ui.end_row();
                for c in checks {
                    ui.label(RichText::new(c.name).size(12.0).color(TEXT));
                    let value = if c.ok {
                        format!("{:#010X}", c.computed)
                    } else {
                        format!("{:#010X} → {:#010X}", c.stored, c.computed)
                    };
                    ui.label(
                        RichText::new(value)
                            .font(FontId::monospace(12.0))
                            .color(MUTED),
                    );
                    ui.label(
                        RichText::new(if c.ok { "OK" } else { "BAD" })
                            .size(12.0)
                            .color(if c.ok { OK } else { DANGER }),
                    );
                    ui.end_row();
                }
            });
    });
}

fn graph_card(
    ui: &mut Ui,
    enabled: &[bool],
    values: &[f64],
    histories: &[Vec<f32>],
    selected: &mut usize,
) {
    card(ui, ui.available_width(), |ui| {
        let active: Vec<usize> = (0..LIVE_VARS.len()).filter(|i| enabled[*i]).collect();
        if active.is_empty() {
            section_title(ui, "Graph");
            ui.label(RichText::new("No variables selected.").color(MUTED));
            return;
        }
        if !active.contains(selected) {
            *selected = active[0];
        }
        ui.horizontal(|ui| {
            section_title(ui, "Graph");
            ui.add_space(8.0);
            egui::ComboBox::from_id_salt("graph-var")
                .selected_text(LIVE_VARS[*selected].name)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for &i in &active {
                        ui.selectable_value(selected, i, LIVE_VARS[i].name);
                    }
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{:.1} {}",
                        values[*selected], LIVE_VARS[*selected].unit
                    ))
                    .size(18.0)
                    .strong()
                    .color(ACCENT),
                );
            });
        });
        let h = &histories[*selected];
        let height = 230.0;
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, Color32::from_rgb(10, 15, 19));
        for k in 0..=4 {
            let y = rect.top() + rect.height() * k as f32 / 4.0;
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0_f32, Color32::from_rgb(27, 37, 45)),
            );
        }
        if h.len() >= 2 {
            let min = h.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = h.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let span = (max - min).max(max.abs() * 0.02).max(1e-3);
            let mid = (max + min) * 0.5;
            let pts: Vec<egui::Pos2> = h
                .iter()
                .enumerate()
                .map(|(k, v)| {
                    let x = rect.left()
                        + rect.width() * k as f32 / (h.len().saturating_sub(1)).max(1) as f32;
                    let y = rect.bottom() - rect.height() * (((*v - mid) / span) + 0.5);
                    egui::pos2(x, y)
                })
                .collect();
            painter.add(egui::Shape::line(pts, Stroke::new(1.8_f32, ACCENT)));
            painter.text(
                egui::pos2(rect.left() + 8.0, rect.top() + 6.0),
                egui::Align2::LEFT_TOP,
                format!("{max:.1} {}", LIVE_VARS[*selected].unit),
                FontId::proportional(10.0),
                MUTED,
            );
            painter.text(
                egui::pos2(rect.left() + 8.0, rect.bottom() - 6.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{min:.1} {}", LIVE_VARS[*selected].unit),
                FontId::proportional(10.0),
                MUTED,
            );
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "waiting for samples",
                FontId::proportional(12.0),
                MUTED,
            );
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} samples · {}",
                h.len(),
                LIVE_VARS[*selected].group
            ))
            .size(11.0)
            .color(MUTED),
        );
    });
}

fn live_card(ui: &mut Ui, var: &LiveVar, value: f64, history: &[f32], width: f32) {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(12)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(width - 28.0);
            ui.label(RichText::new(var.name).size(12.0).color(MUTED));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{value:.1}"))
                        .font(FontId::proportional(28.0))
                        .strong()
                        .color(TEXT),
                );
                ui.label(RichText::new(var.unit).size(12.0).color(MUTED));
            });
            let spark_w = ui.available_width().max(140.0);
            let rect = ui
                .allocate_exact_size(Vec2::new(spark_w, 34.0), egui::Sense::hover())
                .0;
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, Color32::from_rgb(12, 17, 21));
            if history.len() > 1 {
                let min = history.iter().cloned().fold(f32::INFINITY, f32::min);
                let max = history.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let span = (max - min).max(1.0);
                let pts: Vec<egui::Pos2> = history
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let x = rect.left() + rect.width() * i as f32 / (history.len() - 1) as f32;
                        let y = rect.bottom() - rect.height() * (*v - min) / span;
                        egui::pos2(x, y)
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, Stroke::new(1.5_f32, ACCENT)));
            }
        });
}

fn default_path(name: &str) -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/{name}"))
        .unwrap_or_else(|_| name.to_string())
}

/// Default run-log directory: `$XDG_DATA_HOME/dde3-toolkit/logs`, falling
/// back to `~/.local/share/dde3-toolkit/logs` (or `./dde3-toolkit-logs` when
/// there is no HOME at all).
fn default_log_dir() -> PathBuf {
    let home = std::env::var_os("HOME");
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| PathBuf::from(h).join(".local/share")));
    match base {
        Some(b) => b.join("dde3-toolkit/logs"),
        None => PathBuf::from("dde3-toolkit-logs"),
    }
}

/// Local wall clock, now. Returns `(compact, human)`:
/// - `compact` — `YYYYmmdd-HHMMSS`, safe for a file name (the run-log name);
/// - `human`   — `YYYY-MM-DD HH:MM:SS.mmm`, for the line prefixes.
fn now_local() -> (String, String) {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let ms = dur.subsec_millis();
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    let (y, mo, d, h, mi, s) = (
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    );
    (
        format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"),
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}"),
    )
}

/// Pre-write battery snapshot (ADF id 0x0F65, DDE3 scale 0.02037243 / LSB).
fn snapshot_battery(k: &mut Kline, tx: &Sender<Msg>) {
    if let Ok(values) = k.live_values(&[(0x0F, 0x65)], true) {
        if let Some(raw) = values.first() {
            let v = *raw as f64 * 0.02037243;
            let _ = tx.send(Msg::Log(format!("BATTERY pre-write {v:.2} V (raw {raw})")));
            let _ = tx.send(Msg::BatterySnapshot(v));
        }
    }
}

fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn report_error(tx: &Sender<Msg>, prefix: &str, err: &anyhow::Error) {
    let text = format!("{prefix}: {err}");
    if protocol::is_link_error(err) {
        let _ = tx.send(Msg::LinkLost(text));
    } else {
        let _ = tx.send(Msg::Error(text));
    }
}

fn pad_calibration(cal: &[u8]) -> Vec<u8> {
    let mut full = vec![0xFFu8; IMAGE_SIZE];
    full[CAL_START..CAL_START + CAL_LEN].copy_from_slice(cal);
    full
}

/// Producer-side polling-rate estimator for the live loop. Records a
/// cycle-completion stamp and returns the TRUE mean rate (Hz) over the last
/// second of cycles: (n-1) intervals divided by the window span. Returns
/// 0.0 until two samples exist.
///
/// Must be called from the live thread, where the cycle time is real: the
/// UI thread processes `Msg::Live` in frame-sized bursts (~60 fps), so a
/// consumer-side `dt` spikes to ~1/dt≈∞ whenever two messages land in one
/// frame — the old counter read "unrealistically high" at fast baud for
/// exactly that reason. Being a plain window average, it cannot overshoot
/// the real rate and converges within ~1 s of starting.
fn live_rate_update(stamps: &mut VecDeque<Instant>, now: Instant) -> f32 {
    stamps.push_back(now);
    while let Some(&front) = stamps.front() {
        if now.duration_since(front) > Duration::from_secs(1) {
            stamps.pop_front();
        } else {
            break;
        }
    }
    match stamps.front() {
        Some(&first) if stamps.len() >= 2 => {
            (stamps.len() - 1) as f32 / now.duration_since(first).as_secs_f32().max(1e-3)
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests;
