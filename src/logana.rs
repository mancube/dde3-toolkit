//! Log analysis — Rust ports of the former Python log-analysis tools
//! (`compare_captures.py`, `analyze_run_log.py`, `analyze_realcar_write.py`;
//! the Python originals were removed after byte-identical parity was
//! confirmed), exposed as `dde3-toolkit log …`:
//!
//! - `log sum <capture.txt> [more.txt …]` — summarize the request sequences
//!   of respond/capture (fake-ECU) logs.
//! - `log run <log> [--image <bin>] [--pairs] [--class <NAME>]` — analyze a
//!   toolkit run log: TX/RX pairing, envelope validation, per-class summary,
//!   FLASH_LESEN sequence + image verify, live-loop timing.
//! - `log write <log> [--mpps <file>] [--lines N]` — analyze a real-car
//!   write run log: note/error timeline, per-class TX pacing, TX→RX latency,
//!   MPPS reference comparison, flash-write phase breakdown.
//!
//! Output is line-for-line the same as the Python tools.

use std::collections::HashMap;
use std::fs;

pub fn run(args: &[String]) -> Result<(), String> {
    let mode = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    match mode {
        "sum" => sum(rest),
        "run" => run_log(rest),
        "write" => write_log(rest),
        _ => Err(usage().to_string()),
    }
}

pub fn usage() -> &'static str {
    "log — analyze dde3-toolkit logs (Rust ports of the Python log tools)\n\n\
     Usage:\n\
       dde3-toolkit log sum <capture.txt> [more.txt …]\n\
           Summarize the request sequences of respond/capture logs.\n\
       dde3-toolkit log run <log> [--image <bin>] [--pairs] [--class <NAME>]\n\
           Analyze a toolkit run log (dde3-*.log): pairing, envelope, per-class\n\
           summary, FLASH_LESEN sequence (+ image verify), live-loop timing.\n\
       dde3-toolkit log write <log> [--mpps <file>] [--lines N]\n\
           Analyze a real-car write run log: note/error timeline, per-class TX\n\
           pacing, TX→RX latency, MPPS reference comparison, write phase."
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Parse a line of the respond/capture log format:
/// `[   12.345] RX|TX|ECHO <hex bytes> [label …]`
fn parse_loga_line(line: &str) -> Option<(f64, &'static str, Vec<u8>)> {
    let rest = line.trim_start().strip_prefix('[')?;
    let close = rest.find(']')?;
    let ts: f64 = rest[..close].trim().parse().ok()?;
    let rest = rest[close + 1..].trim_start();
    let (dir, rest) = if let Some(r) = rest.strip_prefix("RX ") {
        ("RX", r)
    } else if let Some(r) = rest.strip_prefix("TX ") {
        ("TX", r)
    } else if let Some(r) = rest.strip_prefix("ECHO ") {
        ("ECHO", r)
    } else {
        return None;
    };
    let mut bytes = Vec::new();
    for tok in rest.split_whitespace() {
        match u8::from_str_radix(tok, 16) {
            Ok(b) => bytes.push(b),
            Err(_) => break, // label starts here
        }
    }
    if bytes.is_empty() {
        return None;
    }
    Some((ts, dir, bytes))
}

fn hex_up(b: &[u8]) -> String {
    b.iter()
        .map(|c| format!("{:02X}", c))
        .collect::<Vec<_>>()
        .join(" ")
}

fn hex_lo(b: &[u8]) -> String {
    b.iter().map(|c| format!("{:02x}", c)).collect()
}

fn hex_lo_sp(b: &[u8]) -> String {
    b.iter()
        .map(|c| format!("{:02x}", c))
        .collect::<Vec<_>>()
        .join(" ")
}

fn xor_all(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, &c| a ^ c)
}

/// statistics.median: middle element, or mean of the two middle ones.
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Insertion-ordered counter with `Counter.most_common()` semantics
/// (count desc, ties keep first-seen order).
#[derive(Default)]
struct CounterMap {
    keys: Vec<String>,
    counts: HashMap<String, usize>,
}

impl CounterMap {
    fn inc(&mut self, k: &str) {
        if !self.counts.contains_key(k) {
            self.keys.push(k.to_string());
        }
        *self.counts.entry(k.to_string()).or_insert(0) += 1;
    }
    fn most_common(&self, n: usize) -> Vec<(String, usize)> {
        let mut idx: Vec<usize> = (0..self.keys.len()).collect();
        idx.sort_by_key(|&i| std::cmp::Reverse(self.counts[&self.keys[i]]));
        idx.into_iter()
            .take(n)
            .map(|i| (self.keys[i].clone(), self.counts[&self.keys[i]]))
            .collect()
    }
    /// all items in insertion order (like `dict(counter)`)
    fn items(&self) -> Vec<(String, usize)> {
        self.keys
            .iter()
            .map(|k| (k.clone(), self.counts[k]))
            .collect()
    }
    fn len(&self) -> usize {
        self.keys.len()
    }
}

// ---------------------------------------------------------------------------
// log sum — port of compare_captures.py
// ---------------------------------------------------------------------------

fn classify_sum(b: &[u8]) -> (String, String) {
    if b.len() < 3 {
        return ("?short".to_string(), String::new());
    }
    let sid = b[2];
    let sub = b.get(3).copied();
    let addr = if b.len() > 6 {
        Some(((b[4] as u32) << 16) | ((b[5] as u32) << 8) | b[6] as u32)
    } else {
        None
    };
    match sid {
        0x00 => ("IDENT".into(), String::new()),
        0x0D => ("UPROG_EIN".into(), String::new()),
        0x02 => (
            "AIF_LESEN (02)".into(),
            sub.map(|s| format!("param=0x{s:02X}")).unwrap_or_default(),
        ),
        0x04 => (
            "FS_LESEN (04)".into(),
            sub.map(|s| format!("sub=0x{s:02X}")).unwrap_or_default(),
        ),
        0x05 => (
            "FS_LOESCHEN (05)".into(),
            sub.map(|s| format!("sub=0x{s:02X}")).unwrap_or_default(),
        ),
        0x06 => {
            let a = addr.unwrap_or(0);
            let cnt = b.get(7).copied().unwrap_or(0);
            ("FLASH_LESEN".into(), format!("addr=0x{a:06X} n={cnt}"))
        }
        0x07 => {
            let subn = match sub {
                Some(0x02) => "FLASH_SCHREIBEN",
                Some(0x06) => "FLASH_LOESCHEN",
                Some(0x0E) => "DATENBEREICH_LOESCHEN",
                Some(0x0F) => "FLASH_SCHREIBEN_ENDE",
                Some(0x10) => "STEUERN_TESTPLATZ",
                Some(s) => return (format!("07/0x{s:02X}"), String::new()),
                None => return ("07/?".into(), String::new()),
            };
            let a = addr.unwrap_or(0);
            let detail = if sub == Some(0x02) {
                let n = b.get(7).copied().unwrap_or(0);
                format!("addr=0x{a:06X} n={n}")
            } else {
                addr.map(|a| format!("addr=0x{a:06X}")).unwrap_or_default()
            };
            (subn.into(), detail)
        }
        0x0B => {
            let name = match sub {
                Some(0x03) => "MW_MEAS_BLOCK",
                Some(0x04) => "MW_DIGITAL_IN",
                Some(0x07) => "MW_STEUERN",
                Some(0x10) => "MW_SELECT_LESEN",
                Some(0x11) => "MW_PSG_READ",
                Some(0x12) => "MW_PSG_SELECT",
                Some(s) => return (format!("MW/0x{s:02X}"), String::new()),
                None => return ("MW/?".into(), String::new()),
            };
            (name.into(), String::new())
        }
        0x90 => {
            if b.len() > 6 && b[3..6] == [b'B', b'M', b'W'] {
                ("SEED_KEY_REQ".into(), format!("Z=0x{:02X}", b[6]))
            } else {
                ("KEY_SEND".into(), hex_up(&b[3..b.len().min(7)]))
            }
        }
        0x9F => ("DIAGNOSE_ENDE".into(), String::new()),
        0x9E => ("SESSION_KEEP".into(), String::new()),
        0x0A => ("STATUS_CODIER_CHECKSUMME".into(), String::new()),
        0x91 => ("BAUD_UMSTELLUNG".into(), String::new()),
        s => {
            let name = format!("sid=0x{s:02X}");
            let detail = sub.map(|s| format!("sub=0x{s:02X}")).unwrap_or_default();
            (name, detail)
        }
    }
}

fn sum(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(usage().to_string());
    }
    for path in args {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("cannot read {path}: {e}"))?;
        let reqs: Vec<Vec<u8>> = content
            .lines()
            .filter_map(|line| {
                let (ts, dir, b) = parse_loga_line(line)?;
                let _ = ts;
                (dir == "RX").then_some(b)
            })
            .collect();
        println!();
        println!("{}", "=".repeat(78));
        println!("{}  —  {} requests", path, reqs.len());
        println!("{}", "=".repeat(78));
        let mut i = 0;
        while i < reqs.len() {
            let (kind, detail) = classify_sum(&reqs[i]);
            if kind == "FLASH_LESEN" || kind == "FLASH_SCHREIBEN" {
                let mut j = i;
                let mut addrs: Vec<(u32, u32)> = Vec::new();
                while j < reqs.len() && classify_sum(&reqs[j]).0 == kind {
                    let fb = &reqs[j];
                    let a = ((fb[4] as u32) << 16) | ((fb[5] as u32) << 8) | fb[6] as u32;
                    let n = fb.get(7).copied().unwrap_or(0) as u32;
                    addrs.push((a, n));
                    j += 1;
                }
                let lo = addrs.iter().map(|(a, _)| *a).min().unwrap_or(0);
                let hi = addrs.iter().map(|(a, n)| a + n).max().unwrap_or(0);
                let mut sizes: Vec<u32> = addrs.iter().map(|(_, n)| *n).collect();
                sizes.sort_unstable();
                sizes.dedup();
                let sizes_s = format!(
                    "[{}]",
                    sizes
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!(
                    "  {:<22} x{:<5} span 0x{lo:06X}..0x{hi:06X}  block={sizes_s}",
                    kind,
                    addrs.len()
                );
                i = j;
            } else {
                println!("  {:<22} {}", kind, detail);
                i += 1;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// log run — port of analyze_run_log.py
// ---------------------------------------------------------------------------

const JOB_NAMES: &[(u8, &str)] = &[
    (0x00, "IDENT"),
    (0x02, "AIF_LESEN"),
    (0x04, "FS_LESEN"),
    (0x05, "FS_LOESCHEN"),
    (0x06, "FLASH_LESEN"),
    (0x07, "FLASH_WRITE"),
    (0x0A, "STATUS_CODIER_CHECKSUMME"),
    (0x0B, "MW"),
    (0x0D, "UPROG_EIN"),
    (0x0E, "TEST_STAMP_READ"),
    (0x0F, "TEST_STAMP_WRITE"),
    (0x14, "FS_LESEN_COUNT"),
    (0x40, "ADJUST_READ"),
    (0x41, "ADJUST_SET"),
    (0x42, "ADJUST_STORE"),
    (0x6C, "EWS_SYNC_START"),
    (0x6D, "EWS_SYNC_STATUS"),
    (0x90, "SEED_KEY"),
    (0x91, "BAUD"),
    (0x9E, "SESSION_KEEP"),
    (0x9F, "DIAGNOSE_ENDE"),
];

/// Port of analyze_run_log.classify — frame includes the 0x12 prefix.
fn classify_run(t: &[u8]) -> (String, String) {
    if t.len() < 3 {
        return ("?short".to_string(), String::new());
    }
    let job = t[2];
    let sub = t.get(3).copied();
    let name = JOB_NAMES
        .iter()
        .find(|(j, _)| *j == job)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("0x{job:02X}"));
    match job {
        0x06 => {
            let addr = ((t[4] as u32) << 16) | ((t[5] as u32) << 8) | t[6] as u32;
            let cnt = t
                .get(7)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into());
            ("FLASH_LESEN".into(), format!("addr=0x{addr:06X} count={cnt}"))
        }
        0x07 => {
            let subn = match sub {
                Some(0x02) => "SCHREIBEN",
                Some(0x06) => "LOESCHEN",
                Some(0x0E) => "DATENBEREICH_LOESCHEN",
                Some(0x0F) => "SCHREIBEN_ENDE",
                Some(0x10) => "STEUERN_TESTPLATZ",
                Some(s) => return (format!("FLASH_sub=0x{s:02X}"), String::new()),
                None => return ("FLASH_?".into(), String::new()),
            };
            let addr = if t.len() > 6 {
                ((t[4] as u32) << 16) | ((t[5] as u32) << 8) | t[6] as u32
            } else {
                0
            };
            if sub == Some(0x02) {
                let n = t.get(7).copied().unwrap_or(0);
                ("FLASH_SCHREIBEN".into(), format!("addr=0x{addr:06X} n={n}"))
            } else {
                (format!("FLASH_{subn}"), format!("addr=0x{addr:06X}"))
            }
        }
        0x0B => {
            let subn = match sub {
                Some(0x03) => "MEAS_BLOCK",
                Some(0x04) => "DIGITAL_IN",
                Some(0x07) => "STEUERN_TESTPLATZ",
                Some(0x10) => "MW_SELECT_LESEN",
                Some(0x11) => "PSG_READ",
                Some(0x12) => "PSG_SELECT",
                Some(s) => return (format!("MW_sub=0x{s:02X}"), String::new()),
                None => return ("MW_?".into(), String::new()),
            };
            if sub == Some(0x10) {
                let mut ids = Vec::new();
                let mut i = 4;
                while i + 1 < t.len() {
                    ids.push(format!("{:02X}{:02X}", t[i], t[i + 1]));
                    i += 2;
                }
                ("MW_SELECT_LESEN".into(), ids.join(","))
            } else {
                (format!("MW_{subn}"), String::new())
            }
        }
        0x90 => {
            if t.len() > 6 && t[3..6] == [b'B', b'M', b'W'] {
                ("SEED_REQ".into(), format!("Z=0x{:02X}", t[6]))
            } else {
                ("KEY_SEND".into(), hex_up(&t[3..t.len().min(7)]))
            }
        }
        0x04 => {
            let name = if sub == Some(0) { "FS_ENTRY" } else { "FS_SHADOW" };
            (name.into(), sub.map(|s| format!("sub=0x{s:02X}")).unwrap_or_default())
        }
        _ => {
            let detail = sub.map(|s| format!("sub=0x{s:02X}")).unwrap_or_default();
            (name, detail)
        }
    }
}

/// Port of analyze_run_log.frame_check.
fn frame_check(b: &[u8]) -> (bool, Option<bool>, Option<bool>) {
    // (len_ok, xor_ok, has_xor)
    if b.len() < 3 {
        return (false, None, None);
    }
    let ln = b[1] as usize;
    let has_xor: Option<bool> = if b.len() == ln {
        Some(xor_all(&b[..b.len() - 1]) == b[b.len() - 1])
    } else if b.len() == ln - 1 {
        Some(false)
    } else {
        None
    };
    let len_ok = has_xor.is_some();
    let xor_ok = if has_xor == Some(false) {
        None
    } else {
        has_xor
    };
    (len_ok, xor_ok, has_xor)
}

/// Parse a run-log line:
/// `<YYYY-MM-DD HH:MM:SS.mmm> [+ <gap>s] TX|RX <hex bytes>`
fn parse_run_line(line: &str) -> Option<(String, &'static str, f64, Vec<u8>)> {
    if line.len() < 23 {
        return None;
    }
    let b = line.as_bytes();
    let valid = (0..4).all(|i| b[i].is_ascii_digit())
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
        && b[10] == b' '
        && b[11].is_ascii_digit()
        && b[12].is_ascii_digit()
        && b[13] == b':'
        && b[14].is_ascii_digit()
        && b[15].is_ascii_digit()
        && b[16] == b':'
        && b[17].is_ascii_digit()
        && b[18].is_ascii_digit()
        && b[19] == b'.'
        && b[20].is_ascii_digit()
        && b[21].is_ascii_digit()
        && b[22].is_ascii_digit();
    if !valid {
        return None;
    }
    let ts = line[..23].to_string();
    let rest = line[23..].trim_start();
    let rest = rest.strip_prefix('[')?;
    let rest = rest.strip_prefix('+')?;
    let rest = rest.trim_start();
    let end = rest.find("s]")?;
    let gap: f64 = rest[..end].parse().ok()?;
    let rest = &rest[end + 2..];
    let rest = rest.strip_prefix(' ')?;
    let (dir, rest) = if let Some(r) = rest.strip_prefix("TX ") {
        ("TX", r)
    } else if let Some(r) = rest.strip_prefix("RX ") {
        ("RX", r)
    } else {
        return None;
    };
    let hex = rest.trim_end();
    if hex.is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    for tok in hex.split_whitespace() {
        match u8::from_str_radix(tok, 16) {
            Ok(c) => bytes.push(c),
            Err(_) => return None,
        }
    }
    Some((ts, dir, gap, bytes))
}

/// seconds-of-day from `YYYY-MM-DD HH:MM:SS.mmm` (date ignored, like the
/// Python tool's `ts2s`).
fn ts2s(ts: &str) -> f64 {
    let h: u32 = ts[11..13].parse().unwrap_or(0);
    let m: u32 = ts[14..16].parse().unwrap_or(0);
    let s: u32 = ts[17..19].parse().unwrap_or(0);
    let ms: u32 = ts[20..23].parse().unwrap_or(0);
    h as f64 * 3600.0 + m as f64 * 60.0 + s as f64 + ms as f64 / 1000.0
}

fn run_log(args: &[String]) -> Result<(), String> {
    let mut image: Option<Vec<u8>> = None;
    let mut dump_pairs = false;
    let mut dump_class: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--image" => {
                i += 1;
                let p = args.get(i).ok_or("--image needs a value")?.clone();
                image = Some(
                    fs::read(&p)
                        .map_err(|e| format!("cannot read {p}: {e}"))?,
                );
            }
            "--pairs" => dump_pairs = true,
            "--class" => {
                i += 1;
                dump_class = Some(
                    args.get(i)
                        .ok_or("--class needs a value")?
                        .to_uppercase()
                        .replace("0X", ""),
                );
            }
            a if !a.starts_with("--") && path.is_none() => path = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    let path = path.ok_or_else(|| usage().to_string())?;
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {path}: {e}"))?;

    let mut frames: Vec<(String, &'static str, f64, Vec<u8>)> = Vec::new();
    let mut notes: Vec<(usize, String)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if let Some((ts, dir, gap, b)) = parse_run_line(line) {
            frames.push((ts, dir, gap, b));
        } else if !line.trim().is_empty()
            && !line.starts_with("===")
            && !line.starts_with("started:")
            && !line.starts_with("mode:")
            && !line.starts_with("2026-")
        {
            notes.push((i + 1, line.to_string()));
        } else if line.starts_with("2026-") && !line.contains("TX ") && !line.contains("RX ") {
            notes.push((i + 1, line.to_string()));
        }
    }

    println!("== {path}");
    println!(
        "   frames: {}  ({} TX / {} RX)",
        frames.len(),
        frames.iter().filter(|f| f.1 == "TX").count(),
        frames.iter().filter(|f| f.1 == "RX").count()
    );
    for (i, ln) in &notes {
        println!("   [line {i}] {ln}");
    }

    // Pair TX -> RX (strictly alternating in this log format).
    let mut pairs: Vec<(&[u8], &[u8], f64, f64)> = Vec::new();
    let mut j = 0;
    while j < frames.len() {
        let (ts, d, gap, b) = &frames[j];
        let _ = ts;
        if *d == "TX" && j + 1 < frames.len() && frames[j + 1].1 == "RX" {
            pairs.push((b, &frames[j + 1].3, *gap, frames[j + 1].2));
            j += 2;
        } else {
            println!("   !! unpaired {d}: {}", hex_up(b));
            j += 1;
        }
    }

    // Envelope validation on RX frames.
    let mut bad = 0;
    for (t, r, _, _) in &pairs {
        let (ok, xok, has_xor) = frame_check(r);
        if !ok || (has_xor == Some(true) && xok != Some(true)) {
            bad += 1;
            println!("   !! RX envelope mismatch: TX {}", hex_up(t));
            let lenfield = if r.len() > 1 {
                format!("0x{:02x}", r[1])
            } else {
                "?".into()
            };
            println!(
                "      RX {} (len={}, LEN field={lenfield})",
                hex_up(r),
                r.len()
            );
        }
    }
    println!(
        "   envelope: {}/{len} RX frames validate (LEN field, XOR where present)",
        pairs.len() - bad,
        len = pairs.len()
    );

    // Per-class summary.
    let mut cls: Vec<(String, ClassStat)> = Vec::new();
    for (t, r, _, rgap) in &pairs {
        let (key, detail) = classify_run(t);
        let (lenfield, status) = (
            if r.len() > 1 { r[1] } else { 0 },
            if r.len() > 2 { r[2] } else { 0 },
        );
        let idx = cls.iter().position(|(k, _)| *k == key);
        let e = match idx {
            Some(i) => &mut cls[i].1,
            None => {
                cls.push((key.clone(), ClassStat::default()));
                &mut cls.last_mut().unwrap().1
            }
        };
        e.n += 1;
        e.rx.inc(&format!("({}, {}, {})", r.len(), lenfield, status));
        e.gaps.push(*rgap);
        e.details.inc(&detail);
    }
    println!();
    println!("== per-command summary (RX shape: (len, LENfield, status)) ==");
    for (key, e) in &cls {
        let mut g = e.gaps.clone();
        g.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = g[g.len() / 2];
        let max = g[g.len() - 1];
        let rx_s = format!(
            "{{{}}}",
            e.rx.items()
                .iter()
                .map(|(k, c)| format!("{k}: {c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "  {:<22} n={:<4} rx={rx_s}  reply-gap p50={:.0}ms max={:.0}ms",
            key,
            e.n,
            p50 * 1000.0,
            max * 1000.0
        );
        for (d, c) in e.details.most_common(8) {
            if c > 1 {
                println!("      {d}  x{c}");
            } else {
                println!("      {d}");
            }
        }
        if e.details.len() > 8 {
            println!("      (+{} more distinct)", e.details.len() - 8);
        }
    }

    // Flash-read sequence.
    let reads: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .filter(|(t, _, _, _)| classify_run(t).0 == "FLASH_LESEN")
        .map(|(t, r, _, _)| (*t, *r))
        .collect();
    if !reads.is_empty() {
        println!();
        println!("== FLASH_LESEN sequence: {} reads ==", reads.len());
        let addrs: Vec<(u32, u32)> = reads
            .iter()
            .map(|(t, _)| {
                let a = ((t[4] as u32) << 16) | ((t[5] as u32) << 8) | t[6] as u32;
                let c = t.get(7).copied().unwrap_or(0) as u32;
                (a, c)
            })
            .collect();
        println!(
            "  first: 0x{:06X}  last: 0x{:06X}  span 0x{:06X}..0x{:06X}",
            addrs[0].0,
            addrs.last().unwrap().0,
            addrs[0].0,
            addrs.last().unwrap().0 + addrs.last().unwrap().1
        );
        let mut gaps: Vec<String> = Vec::new();
        for i in 1..addrs.len() {
            let (a0, c0) = addrs[i - 1];
            let (a1, _) = addrs[i];
            if a1 != a0 + c0 {
                gaps.push(format!("'0x{e:06X}->0x{a1:06X}'", e = a0 + c0));
            }
        }
        if gaps.is_empty() {
            println!("  contiguous, no gaps");
        } else {
            println!("  GAPS: [{}]", gaps.iter().take(10).cloned().collect::<Vec<_>>().join(", "));
        }
        if let Some(img) = &image {
            let mut badn = 0;
            for (t, r) in &reads {
                let addr = ((t[4] as u32) << 16) | ((t[5] as u32) << 8) | t[6] as u32;
                let cnt = t.get(7).copied().unwrap_or(0) as usize;
                let mut data = &r[3..];
                if data.len() == cnt + 1 && xor_all(&r[..r.len() - 1]) == r[r.len() - 1] {
                    data = &data[..data.len() - 1];
                }
                let lo = (addr as usize).min(img.len());
                let want = &img[lo..(addr as usize + cnt).min(img.len())];
                if data != want {
                    badn += 1;
                    if badn <= 3 {
                        let disp = &img[lo..(addr as usize + 8).min(img.len())];
                        println!(
                            "  MISMATCH @0x{addr:06X}: reply {} vs image {}",
                            hex_lo(&data[..data.len().min(8)]),
                            hex_lo(disp)
                        );
                    }
                }
            }
            if badn == 0 {
                println!("  vs image: ALL MATCH");
            } else {
                println!("  vs image: {badn} MISMATCHES");
            }
        }
    }

    // Live loop.
    let live: Vec<(&[u8], &[u8], f64, f64)> = pairs
        .iter()
        .filter(|(t, _, _, _)| classify_run(t).0 == "MW_SELECT_LESEN")
        .copied()
        .collect();
    if !live.is_empty() {
        println!();
        println!("== live loop: {} cycles ==", live.len());
        let tss: Vec<f64> = frames
            .iter()
            .filter(|(_, d, _, b)| *d == "TX" && b.len() > 2 && b[2] == 0x0B)
            .map(|(ts, _, _, _)| ts2s(ts))
            .collect();
        if tss.len() > 2 {
            let mut iv: Vec<f64> = (1..tss.len()).map(|i| tss[i] - tss[i - 1]).collect();
            iv = iv[1..iv.len() - 1].to_vec(); // drop first/last (partial)
            if !iv.is_empty() {
                let mut s = iv.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let p50 = s[s.len() / 2];
                println!(
                    "  period: p50={:.0}ms min={:.0}ms max={:.0}ms -> {:.2} Hz",
                    p50 * 1000.0,
                    iv.iter().copied().fold(f64::MAX, |a, b| a.min(b)) * 1000.0,
                    iv.iter().copied().fold(f64::MIN, |a, b| a.max(b)) * 1000.0,
                    1.0 / p50
                );
            }
        }
        let mut seen: CounterMap = CounterMap::default();
        for (_, r, _, _) in &live {
            seen.inc(&hex_up(r));
        }
        println!("  distinct RX frames: {}", seen.len());
        for (k, c) in seen.items().into_iter().take(6) {
            println!("    x{c:<4} {k}");
        }
    }

    if dump_pairs {
        println!();
        println!("== all pairs ==");
        for (t, r, _, rgap) in &pairs {
            let (key, detail) = classify_run(t);
            println!("  TX {}  [{key} {detail}]", hex_up(t));
            println!("  RX {}  (+{:.0}ms)", hex_up(r), rgap * 1000.0);
        }
    }
    if let Some(dc) = &dump_class {
        println!();
        println!("== pairs of class {dc} ==");
        for (t, r, _, rgap) in &pairs {
            let (key, detail) = classify_run(t);
            if key.to_uppercase() == *dc {
                println!("  TX {}  [{detail}]", hex_up(t));
                println!("  RX {}  (+{:.0}ms)", hex_up(r), rgap * 1000.0);
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct ClassStat {
    n: usize,
    rx: CounterMap,
    gaps: Vec<f64>,
    details: CounterMap,
}

// ---------------------------------------------------------------------------
// log write — port of analyze_realcar_write.py
// ---------------------------------------------------------------------------

fn classify_w(payload: &[u8]) -> String {
    // payload = DS2 frame WITHOUT the 0x12 prefix
    if payload.len() < 3 {
        return "short".into();
    }
    let sid = payload[1];
    let sub = payload.get(2).copied();
    let base = match sid {
        0x00 => "IDENT",
        0x0D => "UPROG_EIN",
        0x02 => "AIF_READ",
        0x04 => "FS_LESEN",
        0x05 => "FS_LOESCHEN",
        0x90 => "SEED_KEY",
        0x06 => "FLASH_LESEN",
        0x07 => "FLASH_WRITE",
        0x0B => "LIVE_READ",
        0x0F => "TEST_STAMP",
        0x9F => "DIAGNOSE_ENDE",
        0x10 => "LIVE",
        s => return format!("svc0x{s:02X}"),
    };
    match sid {
        0x07 => {
            let subn = match sub {
                Some(0x02) => "SCHREIBEN",
                Some(0x06) => "LOESCHEN",
                Some(0x0F) => "SCHREIBEN_ENDE",
                Some(s) => return format!("FLASH_WRITE/sub{s:02X}"),
                None => return "FLASH_WRITE/sub?".into(),
            };
            format!("{base}/{subn}")
        }
        0x0B => "LIVE_READ".into(),
        _ => base.into(),
    }
}

fn write_log(args: &[String]) -> Result<(), String> {
    let mut path: Option<String> = None;
    let mut mpps: Option<String> = None;
    let mut max_lines: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mpps" => {
                i += 1;
                mpps = Some(args.get(i).ok_or("--mpps needs a value")?.clone());
            }
            "--lines" => {
                i += 1;
                max_lines = Some(
                    args.get(i)
                        .ok_or("--lines needs a value")?
                        .parse()
                        .map_err(|_| "invalid --lines value".to_string())?,
                );
            }
            a if !a.starts_with("--") && path.is_none() => path = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    let path = path.ok_or_else(|| usage().to_string())?;
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {path}: {e}"))?;

    // parse with delta chaining (absolute stamps are flush times)
    let mut events: Vec<(f64, String, Vec<u8>)> = Vec::new(); // (t, dir, bytes)
    let mut last_frame_t: Option<f64> = None;
    let mut prev_ts: Option<String> = None;
    let mut line_no = 0;
    for line in content.lines() {
        line_no += 1;
        if let Some(ml) = max_lines {
            if line_no > ml {
                break;
            }
        }
        if let Some((ts, dir, gap, b)) = parse_run_line(line) {
            let t = match (last_frame_t, prev_ts.as_deref()) {
                (None, _) => ts2s(&ts),
                (Some(_), Some(pt)) if gap == 0.0 && ts != pt => ts2s(&ts),
                (Some(lt), _) => lt + gap,
            };
            last_frame_t = Some(t);
            prev_ts = Some(ts);
            events.push((t, dir.to_string(), b));
        } else if let Some((ts, text)) = parse_note_line(line) {
            events.push((ts2s(&ts), "NOTE".to_string(), text.into_bytes()));
        }
    }
    if events.is_empty() {
        return Err(format!("no events parsed from {path}"));
    }
    let t0 = events[0].0;
    for e in &mut events {
        e.0 -= t0;
    }
    let frames: Vec<(f64, &str, &[u8])> = events
        .iter()
        .filter(|(_, d, _)| d == "TX" || d == "RX")
        .map(|(t, d, b)| (*t, d.as_str(), b.as_slice()))
        .collect();
    let notes: Vec<(f64, &str)> = events
        .iter()
        .filter(|(_, d, _)| d == "NOTE")
        .map(|(t, _, b)| (*t, std::str::from_utf8(b).unwrap_or("")))
        .collect();

    println!("=== {path} ===");
    println!(
        "events: {}  frames: {}  notes: {}",
        events.len(),
        frames.len(),
        notes.len()
    );
    println!("span: {:.1} s\n", events.last().unwrap().0);

    // ---- 1. timeline of notes + context around each ERROR -----------------
    println!("--- NOTE lines (absolute, rel to start) ---");
    for (t, text) in &notes {
        println!("  [{t:9.3}] {text}");
    }
    println!();

    println!("--- frames in [err-30s, err+3s] around each ERROR ---");
    for (t, text) in &notes {
        if !text.contains("ERROR") {
            continue;
        }
        println!("  !! [{t:9.3}] {text}");
        for (ft, d, b) in &frames {
            if *t - 30.0 <= *ft && *ft <= *t + 3.0 {
                let hex = hex_lo_sp(&b[..b.len().min(10)]);
                let more = if b.len() > 10 { "..." } else { "" };
                println!("     [{ft:9.3}] {d} {hex}{more}");
            }
        }
    }
    println!();

    // ---- 2. per-class TX pacing -------------------------------------------
    println!("--- our run: per-class TX stats (gap to next TX of any class) ---");
    let tx: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, (_, d, _))| *d == "TX")
        .map(|(i, _)| i)
        .collect();
    let mut by_class: HashMap<String, Vec<(f64, Option<f64>)>> = HashMap::new();
    for (k, &i) in tx.iter().enumerate() {
        let (t, _, b) = &frames[i];
        let name = classify_w(&b[1..]);
        let gap_next = tx.get(k + 1).map(|&j| frames[j].0 - *t);
        by_class.entry(name).or_default().push((*t, gap_next));
    }
    let mut class_names: Vec<&String> = by_class.keys().collect();
    class_names.sort();
    for name in class_names {
        let mut rows = by_class[name].clone();
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let gaps: Vec<f64> = rows.iter().filter_map(|(_, g)| *g).collect();
        if gaps.is_empty() {
            println!("  {name:22} n={:4}", rows.len());
        } else {
            println!(
                "  {name:22} n={:4}  gap min={:7.0}ms med={:7.0}ms max={:7.0}ms",
                rows.len(),
                gaps.iter().copied().fold(f64::MAX, |a, b| a.min(b)) * 1000.0,
                median(&gaps) * 1000.0,
                *gaps.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap() * 1000.0
            );
        }
    }
    println!();

    // ---- 3. TX->RX reply latency per class --------------------------------
    println!("--- our run: TX->RX reply latency (first RX before next TX) ---");
    let rx: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, (_, d, _))| *d == "RX")
        .map(|(i, _)| i)
        .collect();
    let mut lat: HashMap<String, Vec<f64>> = HashMap::new();
    let mut unpaired = 0;
    for (k, &i) in tx.iter().enumerate() {
        let (t, _, b) = &frames[i];
        let name = classify_w(&b[1..]);
        let nxt = tx.get(k + 1).map(|&j| frames[j].0);
        let mut found = false;
        for &ri in &rx {
            let tr = frames[ri].0;
            // strictly after: a +0.000s-delta line shares its time with the
            // previous line, which may be the PREVIOUS frame's reply
            if tr <= *t {
                continue;
            }
            if let Some(n) = nxt {
                if tr > n {
                    break;
                }
            }
            lat.entry(name).or_default().push(tr - t);
            found = true;
            break;
        }
        if !found {
            unpaired += 1;
        }
    }
    if unpaired > 0 {
        println!("  ({unpaired} TX frames had no reply before the next TX)");
    }
    let mut lat_names: Vec<&String> = lat.keys().collect();
    lat_names.sort();
    for name in lat_names {
        let mut v = lat[name].clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p90 = v[(v.len() as f64 * 0.9) as usize];
        println!(
            "  {name:22} n={:4}  med={:7.0}ms p90={:7.0}ms max={:7.0}ms",
            v.len(),
            median(&v) * 1000.0,
            p90 * 1000.0,
            v[v.len() - 1] * 1000.0
        );
    }
    println!();

    // ---- 4. MPPS reference pacing -----------------------------------------
    if let Some(mp) = &mpps {
        let mc = fs::read_to_string(mp)
            .map_err(|e| format!("cannot read {mp}: {e}"))?;
        let mut mev: Vec<(f64, &'static str, Vec<u8>)> = Vec::new();
        for line in mc.lines() {
            if let Some(e) = parse_loga_line(line) {
                mev.push(e);
            }
        }
        let mpps_tx: Vec<(f64, Vec<u8>)> = mev
            .iter()
            .filter(|(_, d, _)| *d == "RX")
            .map(|(t, _, b)| (*t, b.clone()))
            .collect();
        let mpps_rx: Vec<(f64, Vec<u8>)> = mev
            .iter()
            .filter(|(_, d, _)| *d == "TX")
            .map(|(t, _, b)| (*t, b.clone()))
            .collect();
        if let Some(&mt0) = mpps_tx.first().map(|e| e.0).as_ref() {
            let mpps_tx: Vec<(f64, Vec<u8>)> =
                mpps_tx.into_iter().map(|(t, b)| (t - mt0, b)).collect();
            let mpps_rx: Vec<(f64, Vec<u8>)> =
                mpps_rx.into_iter().map(|(t, b)| (t - mt0, b)).collect();
            println!("=== MPPS reference: {mp} ===");
            println!(
                "  MPPS frames: {}  span: {:.1}s",
                mpps_tx.len(),
                mpps_tx.last().map(|e| e.0).unwrap_or(0.0)
            );
            println!("  --- MPPS per-class TX stats ---");
            let mut mcls: HashMap<String, Vec<Option<f64>>> = HashMap::new();
            for (k, e) in mpps_tx.iter().enumerate() {
                let name = classify_w(&e.1[1..]);
                let gap = mpps_tx.get(k + 1).map(|e2| e2.0 - e.0);
                mcls.entry(name).or_default().push(gap);
            }
            let mut cn: Vec<&String> = mcls.keys().collect();
            cn.sort();
            for name in cn {
                let gaps: Vec<f64> = mcls[name].iter().copied().flatten().collect();
                if !gaps.is_empty() {
                    println!(
                        "  {name:22} n={:4}  gap min={:7.0}ms med={:7.0}ms max={:7.0}ms",
                        gaps.len(),
                        gaps.iter().copied().fold(f64::MAX, |a, b| a.min(b)) * 1000.0,
                        median(&gaps) * 1000.0,
                        *gaps.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap() * 1000.0
                    );
                }
            }
            println!("  --- MPPS TX->reply latency ---");
            let mut mlat: HashMap<String, Vec<f64>> = HashMap::new();
            let mut j = 0usize;
            for (k, e) in mpps_tx.iter().enumerate() {
                let name = classify_w(&e.1[1..]);
                let nxt = mpps_tx.get(k + 1).map(|e2| e2.0);
                let mut k2 = j;
                while k2 < mpps_rx.len() {
                    let tr = mpps_rx[k2].0;
                    if tr < e.0 {
                        k2 += 1;
                        continue;
                    }
                    if let Some(n) = nxt {
                        if tr > n {
                            break;
                        }
                    }
                    j = k2;
                    mlat.entry(name).or_default().push(tr - e.0);
                    break;
                }
            }
            let mut ln: Vec<&String> = mlat.keys().collect();
            ln.sort();
            for name in ln {
                let mut v = mlat[name].clone();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!(
                    "  {name:22} n={:4}  med={:7.0}ms max={:7.0}ms",
                    v.len(),
                    median(&v) * 1000.0,
                    v[v.len() - 1] * 1000.0
                );
            }
            println!();
        }
    }

    // ---- 5. write phase breakdown (our run) -------------------------------
    println!("--- our run: FLASH write phase timeline (first/last, total) ---");
    let w: Vec<(f64, &[u8])> = frames
        .iter()
        .filter(|(_, d, b)| *d == "TX" && b.len() > 4 && b[2] == 0x07)
        .map(|(t, _, b)| (*t, *b))
        .collect();
    if !w.is_empty() {
        let mut kinds: HashMap<String, Vec<f64>> = HashMap::new();
        for (t, b) in &w {
            let k = if b[3] == 0x02 {
                "data".to_string()
            } else {
                format!("sub{:02X}", b[3])
            };
            kinds.entry(k).or_default().push(*t);
        }
        let mut kn: Vec<&String> = kinds.keys().collect();
        kn.sort();
        for k in kn {
            let ts_ = &kinds[k];
            println!(
                "  {k:8} n={:4}  first={:.1}s last={:.1}s span={:.1}s",
                ts_.len(),
                ts_.first().unwrap(),
                ts_.last().unwrap(),
                ts_.last().unwrap() - ts_.first().unwrap()
            );
        }
        if let Some(data) = kinds.get("data") {
            if data.len() > 2 {
                let iv: Vec<f64> = (1..data.len()).map(|i| data[i] - data[i - 1]).collect();
                println!(
                    "  data-frame cadence: med={:.0}ms min={:.0}ms max={:.0}ms",
                    median(&iv) * 1000.0,
                    iv.iter().copied().fold(f64::MAX, |a, b| a.min(b)) * 1000.0,
                    iv.iter().copied().fold(f64::MIN, |a, b| a.max(b)) * 1000.0
                );
                println!(
                    "  => {} frames in {:.1}s = {:.0} ms/frame",
                    data.len(),
                    data.last().unwrap() - data.first().unwrap(),
                    (data.last().unwrap() - data.first().unwrap())
                        / data.len() as f64
                        * 1000.0
                );
            }
        }
    }
    println!();

    // ---- 6. silence gaps > 5s between frames ------------------------------
    println!("--- gaps > 5s between consecutive frames ---");
    for i in 1..frames.len() {
        let (t1, _, _) = &frames[i - 1];
        let (t2, d2, b2) = &frames[i];
        let g = t2 - t1;
        if g > 5.0 {
            println!(
                "  {t1:9.3} -> {t2:9.3}  ({g:7.1}s)  next: {d2} {}",
                hex_lo_sp(&b2[..b2.len().min(8)])
            );
        }
    }
    Ok(())
}

/// NOTE line: `<ts> <text>` (not a frame line, not a header).
fn parse_note_line(line: &str) -> Option<(String, String)> {
    if line.starts_with("===")
        || line.starts_with("started:")
        || line.starts_with("mode:")
    {
        return None;
    }
    if line.len() < 23 {
        return None;
    }
    let b = line.as_bytes();
    let valid = (0..4).all(|i| b[i].is_ascii_digit())
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
        && b[10] == b' '
        && b[11].is_ascii_digit()
        && b[12].is_ascii_digit()
        && b[13] == b':'
        && b[14].is_ascii_digit()
        && b[15].is_ascii_digit()
        && b[16] == b':'
        && b[17].is_ascii_digit()
        && b[18].is_ascii_digit()
        && b[19] == b'.'
        && b[20].is_ascii_digit()
        && b[21].is_ascii_digit()
        && b[22].is_ascii_digit();
    if !valid {
        return None;
    }
    let ts = line[..23].to_string();
    let text = line[23..].trim().to_string();
    Some((ts, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_line_ok() {
        let l = "2026-11-19 22:22:38.694 [+ 0.442s] RX 12 2E A0 37 37 E7";
        let (ts, dir, gap, b) = parse_run_line(l).unwrap();
        assert_eq!(ts, "2026-11-19 22:22:38.694");
        assert_eq!(dir, "RX");
        assert!((gap - 0.442).abs() < 1e-9);
        assert_eq!(b, vec![0x12, 0x2E, 0xA0, 0x37, 0x37, 0xE7]);
    }

    #[test]
    fn parse_run_line_rejects_garbage() {
        assert!(parse_run_line("=== dde3-toolkit v0.4.1 run log ===").is_none());
        assert!(parse_run_line("started: 2026-11-19 22:22:26.190").is_none());
        assert!(parse_run_line("2026-11-19 22:22:38.694 BATTERY pre-write 12.08 V").is_none());
    }

    #[test]
    fn parse_loga_line_ok() {
        let l = "[    1.920] RX   12 04 00 16   IDENT";
        let (ts, dir, b) = parse_loga_line(l).unwrap();
        assert!((ts - 1.920).abs() < 1e-9);
        assert_eq!(dir, "RX");
        assert_eq!(b, vec![0x12, 0x04, 0x00, 0x16]);
    }

    #[test]
    fn classify_flash_lesen() {
        let t: Vec<u8> = vec![0x12, 0x09, 0x06, 0x00, 0x80, 0x00, 0x00, 0x2E, 0x33];
        let (k, d) = classify_run(&t);
        assert_eq!(k, "FLASH_LESEN");
        assert_eq!(d, "addr=0x800000 count=46");
    }

    #[test]
    fn classify_write_names() {
        // payload without 0x12 prefix
        assert_eq!(classify_w(&[0x05, 0x07, 0x02, 0, 0, 0, 0, 64]), "FLASH_WRITE/SCHREIBEN");
        assert_eq!(classify_w(&[0x05, 0x0B, 0x10, 0x0F, 0x65, 0x64]), "LIVE_READ");
        assert_eq!(classify_w(&[0x04, 0x00, 0x16]), "IDENT");
        assert_eq!(classify_w(&[0x05, 0x33, 0x00]), "svc0x33");
        assert_eq!(classify_w(&[0x03, 0x33]), "short");
    }

    #[test]
    fn frame_check_shapes() {
        // len == LEN field, XOR valid
        let b: Vec<u8> = vec![0x12, 0x04, 0x0A, 0x1C];
        let (ok, xok, hx) = frame_check(&b);
        assert!(ok);
        assert_eq!(hx, Some(true));
        assert_eq!(xok, Some(true));
        // len == LEN-1 (XOR dropped)
        let b: Vec<u8> = vec![0x12, 0x05, 0x0A, 0x00];
        let (ok, xok, hx) = frame_check(&b);
        assert!(ok);
        assert_eq!(hx, Some(false));
        assert_eq!(xok, None);
        // neither
        let b: Vec<u8> = vec![0x12, 0x09, 0x0A, 0x00];
        let (ok, _, hx) = frame_check(&b);
        assert!(!ok);
        assert_eq!(hx, None);
    }
}
