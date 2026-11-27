//! E2E fake-ECU tests — all flash actions against `respond` on a socat
//! pty bridge.
//!
//! **NOT DEFINITIVE.** These drive the real CLI binary against the
//! `respond` fake ECU and prove the tool is self-consistent on the wire
//! (bootstrap, framing, block math, image round-trip). Only on-car runs
//! are the source of truth for what the real ECU accepts — see HANDOVER,
//! "Open / unverified items".
//!
//! **Requires a real 256 KB flash dump** at `captures/actual_flash.bin`
//! (private — never committed to git). This machine has one; everyone else:
//! put your own full 256 KB DDE3 flash dump at that path. Without the file
//! (or with a different size) the tests skip and say so.
#![cfg(unix)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_dde3-toolkit");
const FLASH: &str = "captures/actual_flash.bin";
const SIZE: usize = 262_144; // 256 KB
/// The pty is a byte pipe — baud is nominal, so run the link fast to keep
/// the suite quick (a full read at 9600 takes ~7 minutes).
const BAUD: u32 = 921_600;
const CAL_START: usize = 0x38000;
const CAL_LEN: usize = 0x8000;
const WRITE_START: usize = 0x10000;

/// Load the real flash dump; None (→ skip) when absent or wrong size.
fn flash_image() -> Option<Vec<u8>> {
    match std::fs::read(FLASH) {
        Ok(d) if d.len() == SIZE => Some(d),
        Ok(d) => {
            eprintln!(
                "SKIP: {FLASH} is {} bytes, expected {SIZE} — \
                 provide a full 256 KB DDE3 flash dump there to run the E2E tests",
                d.len()
            );
            None
        }
        Err(_) => {
            eprintln!(
                "SKIP: no flash dump at {FLASH} — \
                 provide your own full 256 KB DDE3 flash dump there to run the E2E tests"
            );
            None
        }
    }
}

fn have_socat() -> bool {
    std::process::Command::new("socat")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Load the flash dump and check socat; skip (None) with a message when a
/// prerequisite is missing.
fn prerequisites(test: &str) -> Option<Vec<u8>> {
    let img = flash_image()?;
    if !have_socat() {
        eprintln!("SKIP {test}: socat not found — install socat to run the E2E tests");
        return None;
    }
    Some(img)
}

/// A pty bridge via socat: two real pty slave paths, bytes copied between
/// them. (A pty *master* has no path on Linux — /proc/<pid>/fd/<fd> for it
/// resolves to the *slave*, so two separate processes can't both reach one
/// pair by path. Two slave ends through socat is the portable equivalent of
/// the manual master/slave quickstart pair.)
fn pty_bridge(tmp: &Tmp) -> (String, String, Child) {
    let a = tmp.path("pty_a");
    let b = tmp.path("pty_b");
    let mut c = Command::new("socat");
    c.args([
        "-d", "-d",
        &format!("pty,raw,echo=0,link={}", a.display()),
        &format!("pty,raw,echo=0,link={}", b.display()),
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = c
        .spawn()
        .expect("spawn socat — install socat to run the E2E tests");
    let t0 = Instant::now();
    while !a.exists() || !b.exists() {
        if t0.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("socat did not create the pty links in time");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (
        a.to_string_lossy().into_owned(),
        b.to_string_lossy().into_owned(),
        child,
    )
}

/// Per-test scratch dir under /tmp, removed on drop.
struct Tmp {
    dir: PathBuf,
}

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dde3-e2e-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.path(name)).unwrap_or_default()
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Spawn the toolkit binary; stdout/stderr go to files (no pipe deadlocks,
/// nothing lost on timeout).
fn spawn_cli(args: &[String], tmp: &Tmp, tag: &str) -> Child {
    let out = std::fs::File::create(tmp.path(&format!("{tag}.out"))).unwrap();
    let err = std::fs::File::create(tmp.path(&format!("{tag}.err"))).unwrap();
    let mut c = Command::new(BIN);
    c.args(args.iter().map(|s| s.as_str()))
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    c.spawn().unwrap()
}

/// Spawn the CLI with a stdin payload (the write gate: "WRITE\n").
fn spawn_cli_stdin(args: &[String], stdin: &str, tmp: &Tmp, tag: &str) -> Child {
    let out = std::fs::File::create(tmp.path(&format!("{tag}.out"))).unwrap();
    let err = std::fs::File::create(tmp.path(&format!("{tag}.err"))).unwrap();
    let mut c = Command::new(BIN);
    c.args(args.iter().map(|s| s.as_str()))
        .stdin(Stdio::piped())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    child
}

fn dump_outputs(tmp: &Tmp, tag: &str) -> (String, String) {
    let out = std::fs::read_to_string(tmp.path(&format!("{tag}.out"))).unwrap_or_default();
    let err = std::fs::read_to_string(tmp.path(&format!("{tag}.err"))).unwrap_or_default();
    (out, err)
}

/// Wait for a child with a deadline; on timeout kill it and panic with its output.
fn wait_or_panic(child: &mut Child, tmp: &Tmp, tag: &str, what: &str, timeout: Duration) -> i32 {
    let t0 = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(st) => break st.code().unwrap_or(-1),
            None => {
                if t0.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let (out, err) = dump_outputs(tmp, tag);
                    panic!("{what} timed out after {timeout:?}\n--- stdout ---\n{out}\n--- stderr ---\n{err}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Wait until the respond banner (port opened) shows up in its output file.
fn wait_ready(child: &mut Child, tmp: &Tmp, tag: &str) {
    let t0 = Instant::now();
    loop {
        let out = std::fs::read_to_string(tmp.path(&format!("{tag}.out"))).unwrap_or_default();
        if out.contains("port=") {
            return;
        }
        if let Some(st) = child.try_wait().unwrap() {
            let (_, err) = dump_outputs(tmp, tag);
            panic!("respond exited early (status {st:?}): {err}");
        }
        if t0.elapsed() > Duration::from_secs(15) {
            panic!("respond did not open its port in time");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// SIGINT the fake ECU (graceful stop — it writes its --dump file) and wait;
/// then tear down the pty bridge.
fn stop_ecu(ecu: &mut Ecu) {
    let pid = ecu.child.id() as i32;
    unsafe { libc::kill(pid, libc::SIGINT) };
    let t0 = Instant::now();
    loop {
        if ecu.child.try_wait().unwrap().is_some() {
            break;
        }
        if t0.elapsed() > Duration::from_secs(15) {
            let _ = ecu.child.kill();
            let _ = ecu.child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = ecu.socat.kill();
    let _ = ecu.socat.wait();
}

/// Start a fake ECU serving `initial` on a fresh pty bridge.
struct Ecu {
    master: String,
    child: Child,
    socat: Child,
    tmp: Tmp,
}

fn start_ecu(initial: &[u8], tag: &str, dump: bool) -> Ecu {
    let tmp = Tmp::new(tag);
    let (master, slave, socat) = pty_bridge(&tmp);
    let img_path = tmp.path("flash.bin");
    std::fs::write(&img_path, initial).unwrap();
    let mut args: Vec<String> = vec![
        "respond".into(),
        "--port".into(),
        slave,
        "--baud".into(),
        BAUD.to_string(),
        "--flash".into(),
        img_path.to_string_lossy().into_owned(),
        "--reply-delay".into(),
        "0".into(),
        "--data-delay".into(),
        "0".into(),
    ];
    if dump {
        args.push("--dump".into());
        args.push(tmp.path("dump.bin").to_string_lossy().into_owned());
    }
    let child = spawn_cli(&args, &tmp, tag);
    let mut ecu = Ecu { master, child, socat, tmp };
    wait_ready(&mut ecu.child, &ecu.tmp, tag);
    ecu
}

/// Run a CLI flash action against the fake ECU and wait for it.
fn run_cli(ecu: &Ecu, args: &[&str], stdin: &str, timeout: Duration) -> i32 {
    let mut args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    args.push("--port".into());
    args.push(ecu.master.clone());
    args.push("--baud".into());
    args.push(BAUD.to_string());
    args.push("--data-delay".into());
    args.push("0".into());
    let tag = "cli";
    let mut child = if stdin.is_empty() {
        spawn_cli(&args, &ecu.tmp, tag)
    } else {
        spawn_cli_stdin(&args, stdin, &ecu.tmp, tag)
    };
    wait_or_panic(&mut child, &ecu.tmp, tag, "CLI command", timeout)
}

#[test]
fn e2e_full_read_round_trip() {
    let img = match prerequisites("e2e_full_read_round_trip") {
        Some(i) => i,
        None => return,
    };
    let mut ecu = start_ecu(&img, "read", false);
    let out = ecu.tmp.path("out.bin");
    let code = run_cli(
        &ecu,
        &["read", &out.to_string_lossy()],
        "",
        Duration::from_secs(300),
    );
    stop_ecu(&mut ecu);
    assert_eq!(code, 0, "read failed");
    let got = ecu.tmp.read("out.bin");
    assert_eq!(got.len(), SIZE, "full read must be 256 KB");
    assert_eq!(got, img, "full read must return the served flash verbatim");
}

#[test]
fn e2e_cal_read_round_trip() {
    let img = match prerequisites("e2e_cal_read_round_trip") {
        Some(i) => i,
        None => return,
    };
    let mut ecu = start_ecu(&img, "readcal", false);
    let out = ecu.tmp.path("out.bin");
    let code = run_cli(
        &ecu,
        &["read-cal", &out.to_string_lossy()],
        "",
        Duration::from_secs(120),
    );
    stop_ecu(&mut ecu);
    assert_eq!(code, 0, "read-cal failed");
    let got = ecu.tmp.read("out.bin");
    assert_eq!(got.len(), CAL_LEN, "cal read must be 32 KB");
    assert_eq!(got, &img[CAL_START..], "cal read must return the served cal area");
}

#[test]
fn e2e_full_write_round_trip() {
    let img = match prerequisites("e2e_full_write_round_trip") {
        Some(i) => i,
        None => return,
    };
    // Start the fake ECU with the writable span zeroed, so "wrote nothing"
    // is distinguishable from "wrote correctly".
    let mut initial = img.clone();
    initial[WRITE_START..].fill(0);
    let mut ecu = start_ecu(&initial, "write", true);
    let img_path = FLASH.to_string();
    let code = run_cli(
        &ecu,
        &["write", &img_path],
        "WRITE\n",
        Duration::from_secs(300),
    );
    stop_ecu(&mut ecu);
    assert_eq!(code, 0, "write failed");
    // The full-span dump is exported as a 256 KB image with the unwritten
    // boot area padded 0xFF (respond::dump_image).
    let mut expected = vec![0xFFu8; WRITE_START];
    expected.extend_from_slice(&img[WRITE_START..]);
    let got = ecu.tmp.read("dump.bin");
    assert_eq!(got.len(), SIZE, "full-span dump must be 256 KB");
    assert_eq!(got, expected, "full write must program the image byte for byte");
}

#[test]
fn e2e_cal_write_round_trip() {
    let img = match prerequisites("e2e_cal_write_round_trip") {
        Some(i) => i,
        None => return,
    };
    // Same trick: zero the cal area first.
    let mut initial = img.clone();
    initial[CAL_START..].fill(0);
    let mut ecu = start_ecu(&initial, "writecal", true);
    let cal_path = ecu.tmp.path("cal.bin");
    std::fs::write(&cal_path, &img[CAL_START..]).unwrap();
    let code = run_cli(
        &ecu,
        &["write-cal", &cal_path.to_string_lossy()],
        "WRITE\n",
        Duration::from_secs(120),
    );
    stop_ecu(&mut ecu);
    assert_eq!(code, 0, "write-cal failed");
    // A non-full span is dumped compactly, first to last written byte.
    let got = ecu.tmp.read("dump.bin");
    assert_eq!(got.len(), CAL_LEN, "cal-span dump must be compact 32 KB");
    assert_eq!(got, &img[CAL_START..], "cal write must program the cal area byte for byte");
}
