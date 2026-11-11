//! DDE3 Toolkit GUI — egui desktop application for BMW EDC15 DDE3.0 over K-line.
//!
//! Mirrors dde4-toolkit's app structure: Overview, Read flash, Write flash,
//! Checksums, Fault codes, Live data, Value logger, Protocol log tabs.
//!
//! ⚠️ The live-data response layout is UNRESOLVED for DDE3 (see HANDOVER.md §14).
//! Fault-code read/clear use the DS2 FS_* frames extracted from DDE30DS0.prg
//! (`12 05 14 01` count, `12 05 04 00` entry, `12 05 04 01` shadow,
//! `12 04 05` clear) with the German text tables in `crate::dtc`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Layout, Margin, RichText, ScrollArea, Stroke, TextStyle,
    Ui, Vec2,
};

use crate::checksum::{self, CAL_LEN, CAL_START, IMAGE_SIZE};
use crate::live::{LiveVar, LIVE_VARS};
use crate::protocol::{self, hex, Kline};

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
        if self.rows % 20 == 0 {
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
    Live(Vec<(u16, u16)>),
    LogProgress(usize),
}

pub struct App {
    link: Arc<Mutex<Option<Kline>>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: bool,
    connected: bool,
    ports: Vec<String>,
    port: String,
    echo: bool,
    tab: Tab,
    status: String,
    error: Option<String>,
    ecu: Option<EcuInfo>,
    logs: Vec<String>,
    read_path: String,
    cal_path: String,
    cal_info: Option<(String, usize)>,
    dtcs: Vec<protocol::Dtc>,
    dtc_clear_armed: bool,
    battery_v: Option<f64>,
    post_write_pending: bool,
    write_path: String,
    image: Option<Vec<u8>>,
    image_name: String,
    image_is_partial: bool,
    checks: Vec<checksum::Checksum>,
    comp_edit: Vec<String>,
    progress: f32,
    progress_text: String,
    active_job: Option<Job>,
    live_running: bool,
    live_stop: Arc<AtomicBool>,
    live_values: Vec<f64>,
    live_history: Vec<Vec<f32>>,
    live_enabled: Vec<bool>,
    live_last: Instant,
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
    fast_write: bool,
    auto_live: bool,
    auto_read: bool,
    auto_cal: bool,
    auto_log: Option<PathBuf>,
    auto_write_cal: bool,
    auto_write: bool,
}

impl App {
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
        fast_write: bool,
    ) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = unbounded();
        let mut app = Self {
            link: Arc::new(Mutex::new(None)),
            tx,
            rx,
            busy: false,
            connected: false,
            ports: Vec::new(),
            port: String::new(),
            echo: true,
            tab: initial_tab,
            status: "Ready".into(),
            error: None,
            ecu: None,
            logs: vec!["DDE3 Toolkit started".into()],
            read_path: default_path("dde3_full.bin"),
            cal_path: default_path("dde3_cal.bin"),
            cal_info: None,
            dtcs: Vec::new(),
            dtc_clear_armed: false,
            battery_v: None,
            post_write_pending: false,
            write_path: String::new(),
            image: None,
            image_name: String::new(),
            image_is_partial: false,
            checks: Vec::new(),
            comp_edit: Vec::new(),
            progress: 0.0,
            progress_text: String::new(),
            active_job: None,
            live_running: false,
            live_stop: Arc::new(AtomicBool::new(false)),
            live_values: vec![0.0; LIVE_VARS.len()],
            live_history: vec![Vec::new(); LIVE_VARS.len()],
            live_enabled: LIVE_VARS.iter().map(|v| v.default_on).collect(),
            live_last: Instant::now(),
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
            fast_write,
            auto_live,
            auto_read,
            auto_cal,
            auto_log,
            auto_write_cal,
            auto_write,
        };
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
        self.write_path = "demo_dde3.bin".into();
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
                Msg::Error(e) => {
                    eprintln!("ERROR {e}");
                    self.busy = false;
                    self.active_job = None;
                    self.progress_text = "Aborted".into();
                    self.error = Some(e.clone());
                    self.status = "Error".into();
                    self.push_log(format!("ERROR {e}"));
                }
                Msg::LinkLost(e) => {
                    eprintln!("LINK_LOST {e}");
                    self.busy = false;
                    self.active_job = None;
                    self.progress_text = "Link lost".into();
                    self.stop_live();
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
                Msg::LogProgress(rows) => {
                    self.log_rows = rows;
                }
                Msg::Live(vals) => {
                    let now = Instant::now();
                    let dt = now.duration_since(self.live_last).as_secs_f32();
                    self.live_last = now;
                    if dt > 0.0 {
                        let hz = 1.0 / dt;
                        self.live_rate = if self.live_rate == 0.0 {
                            hz
                        } else {
                            self.live_rate * 0.8 + hz * 0.2
                        };
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
        self.logs.push(line);
        if self.logs.len() > 2000 {
            self.logs.drain(0..500);
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
        let path = self.port.clone();
        let echo = self.echo;
        let link = self.link.clone();
        let tx = self.tx.clone();
        self.busy = true;
        self.error = None;
        self.status = "Opening port".into();
        thread::spawn(move || {
            let mut guard = lock_or_recover(&link);
            match Kline::open(&path, 9600, echo) {
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
                            // Checksum-status link probe; its payload is no
                            // longer used for the SW label — the ident string
                            // is the source of truth.
                            let _ = k.read_app(0x7BFB0, 10);
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

    fn disconnect(&mut self) {
        if self.busy {
            self.error =
                Some("Wait for the current operation to finish before disconnecting".into());
            return;
        }
        self.stop_live();
        self.connected = false;
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
        let path = PathBuf::from(self.read_path.clone());
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Reading full flash".into()));
            snapshot_battery(k, &tx);
            k.set_silence(10);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Reading {done} / {total} bytes"),
                ));
            };
            let mut result = k.read_full(&mut progress);
            if let Err(e) = &result {
                if !protocol::is_link_error(e) {
                    let _ = tx.send(Msg::Status("Retrying once with 20 ms silence".into()));
                    thread::sleep(Duration::from_millis(1000));
                    k.set_silence(20);
                    result = k.read_full(&mut progress);
                }
            }
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
        let path = PathBuf::from(self.cal_path.clone());
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Reading calibration".into()));
            snapshot_battery(k, &tx);
            k.set_silence(10);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Reading calibration {done} / {total} bytes"),
                ));
            };
            let mut result = k.read_range(CAL_START, CAL_LEN, &mut progress);
            if let Err(e) = &result {
                if !protocol::is_link_error(e) {
                    let _ = tx.send(Msg::Status("Retrying once with 20 ms silence".into()));
                    thread::sleep(Duration::from_millis(1000));
                    k.set_silence(20);
                    result = k.read_range(CAL_START, CAL_LEN, &mut progress);
                }
            }
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
        let fast = self.fast_write;
        self.stop_live();
        self.active_job = Some(Job::Write);
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Erasing and writing".into()));
            snapshot_battery(k, &tx);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Writing {done} / {total} bytes"),
                ));
            };
            k.set_silence(10);
            k.set_write_delay(3);
            // DDE3 K-line is fixed at 9600 8E1; `fast` only drops the
            // inter-byte pause (unverified on real hardware).
            let result = k.write_full(&image, 9600, if fast { 0 } else { 3 }, &mut progress);
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
        let fast = self.fast_write;
        self.stop_live();
        self.active_job = Some(Job::WriteCal);
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let _ = tx.send(Msg::Status("Erasing and writing calibration".into()));
            snapshot_battery(k, &tx);
            let mut progress = |done: usize, total: usize| {
                let _ = tx.send(Msg::Progress(
                    done as f32 / total as f32,
                    format!("Writing calibration {done} / {total} bytes"),
                ));
            };
            k.set_silence(10);
            k.set_write_delay(3);
            // DDE3 K-line is fixed at 9600 8E1; `fast` only drops the
            // inter-byte pause (unverified on real hardware).
            let result = k.write_calibration(&image, 9600, if fast { 0 } else { 3 }, &mut progress);
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
    // Seed/key and fault clear
    // -----------------------------------------------------------------------

    fn send_seed_key(&mut self, z: u8) {
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let req = protocol::frame_seed_key(z);
            match k.request(&req, Duration::from_secs(3)) {
                Ok(r) => {
                    let _ = tx.send(Msg::Log(format!("SEED_KEY Z=0x{z:02X} reply: {}", hex(&r))));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("seed-key failed: {e}")));
                }
            }
        });
    }

    fn send_fault_clear(&mut self) {
        if !self.connected {
            self.error = Some("Connect to the ECU first".into());
            return;
        }
        self.start(move |link, tx| {
            let mut guard = lock_or_recover(&link);
            let Some(k) = guard.as_mut() else {
                let _ = tx.send(Msg::Error("link closed".into()));
                return;
            };
            let req = protocol::frame_fault_clear();
            match k.request(&req, Duration::from_secs(5)) {
                Ok(r) => {
                    let _ = tx.send(Msg::Log(format!("FAULT_CLEAR reply: {}", hex(&r))));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("fault clear failed: {e}")));
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
            .set_file_name("dde3_log.csv")
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
        thread::spawn(move || {
            let mut reported = false;
            while !stop.load(Ordering::Relaxed) {
                let (pairs, logs, link_error) = {
                    let mut guard = lock_or_recover(&link);
                    match guard.as_mut() {
                        Some(k) => {
                            let mut pairs = Vec::new();
                            let mut error = None;
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
                match pairs {
                    Some(v) => {
                        reported = false;
                        let mut rows = None;
                        if let Some(w) = lock_or_recover(&log).as_mut() {
                            w.write_row(&v);
                            rows = Some(w.rows);
                        }
                        if let Some(rows) = rows {
                            let _ = tx.send(Msg::LogProgress(rows));
                        }
                        let _ = tx.send(Msg::Live(v));
                    }
                    None => {}
                }
                thread::sleep(Duration::from_millis(55));
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
                self.write_path = path.display().to_string();
                self.error = None;
                self.status = "Full image loaded".into();
                self.sync_comp_edits();
            }
            Ok(data) if data.len() == CAL_LEN => {
                self.image_is_partial = true;
                self.checks = checksum::verify(&pad_calibration(&data));
                self.image = Some(data);
                self.image_name = path.display().to_string();
                self.write_path = path.display().to_string();
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
                    .stroke(Stroke::new(1.0, BORDER)),
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
                    ui.checkbox(&mut self.echo, "Echo");
                    if self.connected {
                        if centered_button_enabled(
                            ui,
                            !self.busy,
                            Vec2::new(100.0, 30.0),
                            egui::Button::new(RichText::new("Disconnect").color(DANGER))
                                .fill(CARD_ALT),
                        )
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
                ui.add_space(4.0);
                for tab in [
                    Tab::Overview,
                    Tab::Read,
                    Tab::Write,
                    Tab::Checksum,
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
                            Stroke::new(1.0, BORDER)
                        } else {
                            Stroke::NONE
                        })
                        .min_size(Vec2::new(172.0, 38.0));
                    if ui.add(button).clicked() {
                        self.tab = tab;
                        if tab != Tab::Live {
                            self.stop_live();
                        }
                    }
                    ui.add_space(4.0);
                }
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new("v0.1.0").size(10.0).color(MUTED));
                    ui.label(RichText::new("EDC15 / DDE3.0").size(11.0).color(MUTED));
                    ui.separator();
                    if let Some(ecu) = &self.ecu {
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
                });
            });
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
                        .stroke(Stroke::new(1.0, DANGER))
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
                    ui.label(RichText::new("K-line · 9600 8E1").size(12.0).color(TEXT));
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
            .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("ECU image", &["bin"])
                    .pick_file()
                {
                    self.load_image(path);
                }
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
        battery_pill(ui, self.battery_v);
        ui.add_space(4.0);
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
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .animate(true),
                );
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
            .clicked()
            {
                self.start_read_cal();
            }
            if self.active_job == Some(Job::Cal) {
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .animate(true),
                );
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
        battery_pill(ui, self.battery_v);
        ui.add_space(4.0);
        let partial = self.image_is_partial;
        card(ui, ui.available_width(), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Image").color(MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut self.image_name)
                        .desired_width(420.0)
                        .hint_text("select a 256 KB or 32 KB .bin"),
                );
                if centered_button(ui, Vec2::new(90.0, 28.0), egui::Button::new("Open…")).clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ECU image", &["bin"])
                        .pick_file()
                    {
                        self.load_image(path);
                    }
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
                .stroke(Stroke::new(1.0, WARN))
                .corner_radius(10)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.label(RichText::new("Experimental write").color(WARN).strong().size(13.0));
                    ui.label(RichText::new("Full write erases program and calibration. Partial write erases calibration only. Checksums are corrected automatically before writing. Do not interrupt power.").size(12.0).color(TEXT));
                });
            ui.add_space(8.0);
            ui.checkbox(
                &mut self.fast_write,
                "Fast write (no inter-byte pause; DDE3 stays at 9600 baud)",
            );
            ui.label(RichText::new("Fast mode uses the read-path baud and removes the 3 ms per-byte pacing. Bench-test before relying on it.").size(11.0).color(MUTED));
            ui.add_space(10.0);
            ui.checkbox(
                &mut self.backup_ok,
                "I have a verified full backup of this ECU",
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Type WRITE to confirm").color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.write_confirm).desired_width(120.0));
            });
            ui.add_space(8.0);
            let ready = self.backup_ok && self.write_confirm == "WRITE" && self.image.is_some();
            ui.horizontal_wrapped(|ui| {
                if centered_button_enabled(
                    ui,
                    ready && !partial && self.connected && !self.busy,
                    Vec2::new(180.0, 34.0),
                    egui::Button::new(RichText::new("Erase and write").color(BG)).fill(WARN),
                )
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
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .show_percentage()
                        .animate(true),
                );
                ui.label(RichText::new(&self.progress_text).size(12.0).color(MUTED));
            }
        });
        ui.add_space(14.0);
        let checks = self.visible_checks();
        if !checks.is_empty() {
            checksum_card(ui, &checks);
        }
    }

    fn checksum_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Checksums", "Bosch TDI V3.1 block checksums — EDC15M is the DDE3 (same ECU). Algorithm ported from the verified edc15m-checksum-tool.");
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
            .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("ECU image", &["bin"])
                    .pick_file()
                {
                    self.load_image(path);
                }
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
            if centered_button(ui, Vec2::new(80.0, 28.0), egui::Button::new("Copy")).clicked() {
                ui.ctx().copy_text(self.logs.join("\n"));
            }
            if centered_button(ui, Vec2::new(80.0, 28.0), egui::Button::new("Clear")).clicked() {
                self.logs.clear();
            }
            ui.label(
                RichText::new(format!("{} lines", self.logs.len()))
                    .size(12.0)
                    .color(MUTED),
            );
        });
        ui.add_space(8.0);
        let log_height = (ui.available_height() - 12.0).max(240.0);
        Frame::new()
            .fill(Color32::from_rgb(8, 12, 15))
            .stroke(Stroke::new(1.0, BORDER))
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
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
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.bg_fill = CARD_ALT;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(33, 44, 54);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(38, 52, 63);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.selection.bg_fill = ACCENT_DIM;
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
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
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(inner);
            add(ui);
        });
}

fn section_title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).strong().color(TEXT));
    ui.add_space(8.0);
}

/// Battery status pill (DDE3 ADF 0x0F65 scale 0.02037243 / LSB).
/// Shown on the Read/Write tabs: a weak battery mid-flash is the
/// classic brick cause, worth the glance.
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
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
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
                Stroke::new(1.0, Color32::from_rgb(27, 37, 45)),
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
            painter.add(egui::Shape::line(pts, Stroke::new(1.8, ACCENT)));
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
        .stroke(Stroke::new(1.0, BORDER))
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
                painter.add(egui::Shape::line(pts, Stroke::new(1.5, ACCENT)));
            }
        });
}

fn default_path(name: &str) -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/{name}"))
        .unwrap_or_else(|_| name.to_string())
}

/// Pre-write battery snapshot (ADF id 0x0F65, DDE3 scale 0.02037243 / LSB).
fn snapshot_battery(k: &mut Kline, tx: &Sender<Msg>) {
    if let Ok(values) = k.live_values(&[(0x0F, 0x65)], true) {
        if let Some(raw) = values.first() {
            let v = *raw as f64 * 0.02037243;
            let _ = tx.send(Msg::Log(format!("BATTERY pre-write {v:.2} V (raw {raw})")));
            let _ = tx.send(Msg::Battery(v));
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
