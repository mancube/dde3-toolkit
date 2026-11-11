//! DDE3 Toolkit — command line entry.
//!
//! Mirrors the dde4-toolkit feature set: connect/ident, full flash read,
//! partial read, flash write (erase → write blocks → end), seed/key, fault
//! clear, checksum verify/correct, and protocol logging.
//!
//! Hardware is optional: all frame builders and the checksum module are unit
//! tested without a serial port.
//!
//! Run without arguments to launch the egui GUI. CLI subcommands are available
//! for headless/bench use.

mod app;
mod bench;
mod capture;
mod breakmode;
mod checksum;
mod dtc;
mod live;
mod protocol;
mod sim;

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::checksum::{correct, verify, CAL_LEN, IMAGE_SIZE};
use crate::protocol::Kline;

fn print_usage() {
    eprintln!("Usage: dde3-toolkit [--gui] <command> [options]");
    eprintln!();
    eprintln!("With no arguments, launches the egui GUI.");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  ident                      Connect and identify the ECU (12 04 00)");
    eprintln!("  read <out.bin>             Read full 256 KB flash to out.bin");
    eprintln!("  read-cal <out.bin>         Read calibration area (0x38000-0x3FFFF) to out.bin");
    eprintln!("  write <image.bin>          Erase + write full 256 KB flash image");
    eprintln!("  write-cal <cal.bin>        Erase + write calibration area only");
    eprintln!("  seed-key <Z>               Send seed/key init (12 08 90 ... 05 <Z>)");
    eprintln!(
        "  fault-clear                Clear fault memory (K-line BAP 12 09 07 0E 42 41 50 00)"
    );
    eprintln!("  dtc                        Read DS2 fault memory with texts and freeze frames");
    eprintln!("  ds2-clear                  Clear DS2 fault memory (12 04 05)");
    eprintln!("  capture                  PASSIVELY capture K-line traffic for debugging:");
    eprintln!("                             frames named, raw bytes kept verbatim. Read-only");
    eprintln!("                             by construction - nothing is ever transmitted,");
    eprintln!("                             so no erase/write is possible in this mode.");
    eprintln!("                             Default port is the passive tap /dev/ttyUSB0.");
    eprintln!("    [--out <file>] [--duration <secs>]");
    eprintln!("  bench                      Headless run: identify + optional read/write,");
    eprintln!("                             progress and timing on stderr, useful exit status");
    eprintln!("    [--info|--read|--read-cal|--write|--write-cal]");
    eprintln!("    [--image <path>] [--out <path>]");
    eprintln!("  checksum <image.bin>       Verify and optionally correct flash checksums");
    eprintln!("  uprog-ein                  Enter programming mode (12 04 0D)");
    eprintln!("  break-probe              Enter the break-entered service protocol, send one
                             probe byte, dump the response (--fast for 38.4k)");
    eprintln!("  diagnose-ende              Leave session / end diagnostics (12 04 9F)");
    eprintln!("  baud                       Switch the DS2 link to 38.4 kbaud via service 0x91");
    eprintln!("                             (requires the 0x91 flash patch; see");
    eprintln!("                             ~/Desktop/edc15m_out/baud91_patch/PATCH_0x91.md)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --port <path>              Serial port (default /dev/ttyUSB1 FT232R sender;");
    eprintln!("                             capture defaults to /dev/ttyUSB0 passive tap)");
    eprintln!("  --duration <secs>          Capture duration in seconds (capture only;");
    eprintln!("                             default: run until Ctrl-C)");
    eprintln!("  --baud <rate>              Baud rate (default 9600)");
    eprintln!("  --data-delay <ms>          Per-byte delay for control frames (default 3)");
    eprintln!("  --sim                      Run against the built-in ECU simulator");
    eprintln!("                             (frames still go over the wire; the ECU reply");
    eprintln!("                             is simulated because the monitor adapter is passive)");
    eprintln!();
    eprintln!("Safety: A write requires typing WRITE to confirm. Checksums are NOT");
    eprintln!("auto-corrected for DDE3 yet — the algorithm is unverified.");
}

fn parse_hex_u8(s: &str) -> Result<u8> {
    u8::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .with_context(|| format!("invalid byte value: {s}"))
}

fn parse_hex_u32(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .with_context(|| format!("invalid address value: {s}"))
}

fn read_image(path: &str, expected: usize) -> Result<Vec<u8>> {
    let data = std::fs::read(path).with_context(|| format!("cannot read image file {path}"))?;
    if data.len() != expected {
        anyhow::bail!(
            "image {path} is {} bytes, expected {} ({})",
            data.len(),
            expected,
            path
        );
    }
    Ok(data)
}

fn confirm_write() -> Result<()> {
    print!("Type WRITE to confirm flash write: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if line.trim() != "WRITE" {
        anyhow::bail!("write cancelled");
    }
    Ok(())
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    let command = &args[1];
    // /dev/ttyUSB1 = FT232R sender (drives the K-line).
    // /dev/ttyUSB0 = UART-LPT passive monitor (ECU-side tap, cannot drive).
    let mut port: Option<String> = None;
    let mut duration: Option<f64> = None;
    let mut baud: u32 = 9600;
    let mut data_delay_ms: u64 = 3;
    let echo: bool = true;
    let mut sim: bool = false;
    let mut fast: bool = false;
    let mut bench_image: Option<String> = None;
    let mut bench_out: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                port = Some(args.get(i).cloned().context("--port requires a value")?);
            }
            "--duration" => {
                i += 1;
                duration = Some(
                    args.get(i)
                        .context("--duration requires a value")?
                        .parse::<f64>()
                        .with_context(|| format!("invalid duration: {}", args[i]))?,
                );
            }
            "--baud" => {
                i += 1;
                baud = args
                    .get(i)
                    .context("--baud requires a value")?
                    .parse::<u32>()
                    .with_context(|| format!("invalid baud rate: {}", args[i]))?;
            }
            "--data-delay" => {
                i += 1;
                data_delay_ms = args
                    .get(i)
                    .context("--data-delay requires a value")?
                    .parse::<u64>()
                    .with_context(|| format!("invalid data delay: {}", args[i]))?;
            }
            "--sim" => sim = true,
            "--fast" => fast = true,
            "--image" => {
                i += 1;
                bench_image = Some(args.get(i).cloned().context("--image requires a value")?);
            }
            "--out" => {
                i += 1;
                bench_out = Some(args.get(i).cloned().context("--out requires a value")?);
            }
            _ => {}
        }
        i += 1;
    }

    // Resolve the port: explicit --port wins; otherwise the sender for the
    // driver commands and the passive tap for `capture` (read-only).
    let port: String = port.unwrap_or_else(|| {
        if command == "capture" {
            "/dev/ttyUSB0".to_string()
        } else {
            "/dev/ttyUSB1".to_string()
        }
    });

    match command.as_str() {
        "capture" => {
            let opts = capture::CaptureOptions {
                port,
                baud,
                out: bench_out.clone().map(PathBuf::from),
                duration: duration.map(Duration::from_secs_f64),
            };
            capture::run(&opts)?;
            Ok(())
        }
        "ident" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let r = k.identify()?;
            println!("IDENT reply: {}", protocol::hex(&r));
            Ok(())
        }
        "read" => {
            let out = args.get(2).context("read requires <out.bin>").cloned()?;
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let mut image = Vec::with_capacity(IMAGE_SIZE);
            let mut addr: u32 = 0;
            let mut done = 0usize;
            while addr < IMAGE_SIZE as u32 {
                let count = 0x80u32.min(IMAGE_SIZE as u32 - addr) as u8;
                let req = crate::protocol::frame_flash_read(addr, count);
                let r = k.request(&req, Duration::from_secs(5))?;
                // Reply layout: 92 <len> A0 <data...> — status byte at [2],
                // data from [3] (decompiled FLASH_LESEN: SB at S1[2],
                // data copied from S1[3]).
                if r.first() != Some(&0x92) || r.get(2) != Some(&0xA0) {
                    anyhow::bail!(
                        "unexpected flash read reply at 0x{addr:06X}: {}",
                        protocol::hex(&r)
                    );
                }
                let data_start = 3;
                if r.len() < data_start + count as usize {
                    anyhow::bail!(
                        "short flash read reply at 0x{addr:06X}: {}",
                        protocol::hex(&r)
                    );
                }
                image.extend_from_slice(&r[data_start..data_start + count as usize]);
                addr += count as u32;
                done += count as usize;
                println!("read 0x{addr:06X} / 0x{:06X} ({} bytes)", IMAGE_SIZE, done);
            }
            std::fs::write(&out, &image).with_context(|| format!("cannot write {out}"))?;
            println!("wrote {} bytes to {out}", image.len());
            Ok(())
        }
        "read-cal" => {
            let out = args
                .get(2)
                .context("read-cal requires <out.bin>")
                .cloned()?;
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let cal_start: u32 = 0x38000;
            let cal_len: usize = 0x8000;
            let mut image = Vec::with_capacity(cal_len);
            let mut addr = cal_start;
            while addr < cal_start + cal_len as u32 {
                let count = 0x80u32.min(cal_len as u32 - (addr - cal_start)) as u8;
                let req = crate::protocol::frame_flash_read(addr, count);
                let r = k.request(&req, Duration::from_secs(5))?;
                if r.first() != Some(&0x92) || r.get(2) != Some(&0xA0) {
                    anyhow::bail!(
                        "unexpected flash read reply at 0x{addr:06X}: {}",
                        protocol::hex(&r)
                    );
                }
                let data_start = 3;
                if r.len() < data_start + count as usize {
                    anyhow::bail!(
                        "short flash read reply at 0x{addr:06X}: {}",
                        protocol::hex(&r)
                    );
                }
                image.extend_from_slice(&r[data_start..data_start + count as usize]);
                addr += count as u32;
            }
            std::fs::write(&out, &image).with_context(|| format!("cannot write {out}"))?;
            println!("wrote {} bytes to {out}", image.len());
            Ok(())
        }
        "write" => {
            let image_path = args.get(2).context("write requires <image.bin>").cloned()?;
            let image = read_image(&image_path, IMAGE_SIZE)?;
            confirm_write()?;
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            // Enter programming mode.
            let _ = k.request(&crate::protocol::frame_uprog_ein(), Duration::from_secs(3))?;
            // Erase.
            let erase_req = crate::protocol::frame_flash_erase(0x00000, 0x00);
            println!("erasing flash...");
            let _ = k.request(&erase_req, Duration::from_secs(60))?;
            // Write blocks.
            let mut off = 0usize;
            while off < image.len() {
                let chunk = &image[off..off + 0x80.min(image.len() - off)];
                let req = crate::protocol::frame_flash_write(off as u32, chunk);
                let _ = k.request(&req, Duration::from_secs(5))?;
                off += chunk.len();
                println!("wrote 0x{off:06X} / 0x{:06X}", IMAGE_SIZE);
            }
            // End write.
            let end_req = crate::protocol::frame_flash_write_end(0x00000);
            let _ = k.request(&end_req, Duration::from_secs(10))?;
            println!("write complete");
            if sim {
                println!("verifying read-back...");
                let rb = k.read_full(&mut |_d, _t| {})?;
                if rb == image {
                    println!("read-back verified: 256 KB matches written image");
                } else {
                    let m = rb.iter().zip(&image).position(|(a, b)| a != b).unwrap_or(0);
                    anyhow::bail!("read-back mismatch at offset 0x{m:X}");
                }
            }
            Ok(())
        }
        "write-cal" => {
            let cal_path = args
                .get(2)
                .context("write-cal requires <cal.bin>")
                .cloned()?;
            let cal = read_image(&cal_path, 0x8000)?;
            confirm_write()?;
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let _ = k.request(&crate::protocol::frame_uprog_ein(), Duration::from_secs(3))?;
            let erase_req = crate::protocol::frame_flash_erase(0x38000, 0x00);
            println!("erasing calibration...");
            let _ = k.request(&erase_req, Duration::from_secs(60))?;
            let mut off = 0usize;
            while off < cal.len() {
                let chunk = &cal[off..off + 0x80.min(cal.len() - off)];
                let req = crate::protocol::frame_flash_write(0x38000 + off as u32, chunk);
                let _ = k.request(&req, Duration::from_secs(5))?;
                off += chunk.len();
                println!("wrote cal 0x{off:06X} / 0x8000");
            }
            let end_req = crate::protocol::frame_flash_write_end(0x38000);
            let _ = k.request(&end_req, Duration::from_secs(10))?;
            println!("calibration write complete");
            if sim {
                println!("verifying calibration read-back...");
                let rb = k.read_range(0x38000, 0x8000, &mut |_d, _t| {})?;
                if rb == cal {
                    println!("read-back verified: 32 KB calibration matches written image");
                } else {
                    let m = rb.iter().zip(&cal).position(|(a, b)| a != b).unwrap_or(0);
                    anyhow::bail!("read-back mismatch at offset 0x{m:X}");
                }
            }
            Ok(())
        }
        "seed-key" => {
            let z_str = args.get(2).context("seed-key requires <Z>").cloned()?;
            let z = parse_hex_u8(&z_str)?;
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let req = crate::protocol::frame_seed_key(z);
            let r = k.request(&req, Duration::from_secs(3))?;
            println!("SEED_KEY reply: {}", protocol::hex(&r));
            Ok(())
        }
        "fault-clear" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let req = crate::protocol::frame_fault_clear();
            let r = k.request(&req, Duration::from_secs(5))?;
            println!("FAULT_CLEAR reply: {}", protocol::hex(&r));
            Ok(())
        }
        "dtc" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let dtcs = k.read_dtcs()?;
            if dtcs.is_empty() {
                println!("no stored fault codes");
                return Ok(());
            }
            for (i, d) in dtcs.iter().enumerate() {
                println!("[{}] {}", i + 1, d.describe());
            }
            Ok(())
        }
        "ds2-clear" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            k.clear_faults_ds2()?;
            println!("DS2 fault memory cleared");
            Ok(())
        }
        "bench" => {
            let mut mode = bench::Mode::Info;
            for a in &args[2..] {
                match a.as_str() {
                    "--read" => mode = bench::Mode::Read,
                    "--read-cal" => mode = bench::Mode::ReadCal,
                    "--write" => mode = bench::Mode::Write,
                    "--write-cal" => mode = bench::Mode::WriteCal,
                    "--info" => mode = bench::Mode::Info,
                    _ => {}
                }
            }
            let args = bench::BenchArgs {
                port,
                image: bench_image.map(PathBuf::from),
                out: bench_out.map(PathBuf::from),
                mode,
                baud,
                data_delay: data_delay_ms,
                sim,
            };
            bench::run(args)
        }
        "checksum" => {
            let image_path = args
                .get(2)
                .context("checksum requires <image.bin>")
                .cloned()?;
            let image = std::fs::read(&image_path)
                .with_context(|| format!("cannot read image file {image_path}"))?;
            if !(image.len() == IMAGE_SIZE || image.len() == CAL_LEN) {
                anyhow::bail!("image {image_path} is {} bytes; expected 32768 (32 KB calibration) or 262144 (256 KB full)", image.len());
            }
            let results = verify(&image);
            if results.is_empty() {
                anyhow::bail!("no EDC15M TDI V3.1 checksum block found in {image_path}");
            }
            for r in &results {
                println!(
                    "checksum {:<12} 0x{:05X} stored {:08X} computed {:08X} {}",
                    r.name,
                    r.pos,
                    r.stored,
                    r.computed,
                    if r.ok { "OK" } else { "BAD" }
                );
            }
            let all_ok = results.iter().all(|r| r.ok);
            if !all_ok {
                let fixed = correct(&image);
                std::fs::write(&image_path, &fixed)
                    .with_context(|| format!("cannot write corrected image {image_path}"))?;
                println!("checksums corrected and written back to {image_path}");
                println!("re-verify with `checksum` before flashing");
            } else {
                println!("all checksums OK");
            }
            if all_ok {
                Ok(())
            } else {
                anyhow::bail!("checksums were bad and have been corrected in place");
            }
        }
        "uprog-ein" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let _ = k.request(&crate::protocol::frame_uprog_ein(), Duration::from_secs(3))?;
            println!("UPROG_EIN sent");
            Ok(())
        }
        "break-probe" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            // Requires a prior DS2 unlock (UPROG_EIN + SEED_KEY); see FINDINGS §4.
            println!(
                "break-entered service probe (fast={}): bauds 9600->{} (handshake),{}",
                fast,
                breakmode::BAUD_10_4K,
                if fast { format!(" -> {}", breakmode::BAUD_38_4K) } else { String::new() }
            );
            match breakmode::enter(&mut k, fast) {
                Ok(mut h) => {
                    // Probe sequence (FINDINGS §4.4): verified opcodes first,
                    // then the legacy 0x51 byte. The expected answers are the
                    // capture reference for the on-wire layout.
                    if let Ok(r) = h.reset_block() {
                        println!("block handshake 02A/11: {}", protocol::hex(&[r.tag, r.data, r.hi]));
                    }
                    if let Ok(c) = h.checksum() {
                        println!("checksum (0x1B): {c:02X}");
                    }
                    if let Ok(a) = h.get_addr() {
                        println!("address (0x04/0x07): {a:04X}");
                    }
                    // Legacy probe: opcode 0x51 clamps to slot 0x50 → default
                    // A reply [00 02 ED 00].
                    h.raw_write(&[0x01, 0x51, 0x00, 0x00])?;
                    let r = h.raw_read(4, Duration::from_millis(500))?;
                    println!("secondary response (0x51 probe): {}", protocol::hex(&r));
                    h.exit();
                }
                Err(e) => println!("break entry failed: {e}"),
            }
            Ok(())
        }
        "diagnose-ende" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let _ = k.request(
                &crate::protocol::frame_diagnose_ende(),
                Duration::from_secs(3),
            )?;
            println!("DIAGNOSE_ENDE sent");
            Ok(())
        }
        "baud" => {
            // DS2 service 0x91 — only answered by patched firmware (the
            // 0x91 trampoline in 0x185C3, see ~/Desktop/edc15m_out/baud91_patch/).
            //
            // Wire semantics (from the patch design, PATCH_0x91.md):
            //   * the ECU switches S0BG to 0x0C (38.4 k) BEFORE it queues
            //     its ACK, so the ACK goes out at the NEW rate. There is
            //     nothing to wait for at the old rate.
            //   * the ACK (92 04 A0) then rides the line at 38.4k and is
            //     drained below; IDENT proves the new rate end-to-end.
            const TARGET: u32 = 38_400;
            let old_baud = baud;
            let mut k = Kline::open(&port, old_baud, echo)?;
            k.set_data_delay(data_delay_ms);
            let frame = crate::protocol::frame_baud(TARGET, 0x03);
            println!("sending 0x91 @ {old_baud}: {}", protocol::hex(&frame));
            // No ACK expected at the old rate; a short window only.
            let _ = k.request(&frame, Duration::from_millis(100));
            std::thread::sleep(Duration::from_millis(100));
            k.set_baud(TARGET)?;
            // Drain the pending ECU ACK / any line noise at the new rate.
            let drained = k.raw_read(256, Duration::from_millis(300))?;
            if !drained.is_empty() {
                println!("drained pending bytes @ {TARGET}: {}", protocol::hex(&drained));
            }
            // Verify the link at the new rate with IDENT.
            let mut last_err = None;
            for _ in 0..3 {
                match k.request(&crate::protocol::frame_ident(), Duration::from_millis(500)) {
                    Ok(r) => {
                        println!("IDENT @ {TARGET}: {}", protocol::hex(&r));
                        println!("DS2 link now runs at {TARGET} (revert to {old_baud} by power cycle)");
                        return Ok(());
                    }
                    Err(e) => last_err = Some(e),
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            // No reply at the new rate: check whether the ECU never switched.
            let _ = k.set_baud(old_baud);
            match k.request(&crate::protocol::frame_ident(), Duration::from_millis(500)) {
                Ok(r) => anyhow::bail!(
                    "ECU still answers @ {old_baud} ({}) — it never switched (stock firmware or patch not applied?)",
                    protocol::hex(&r)
                ),
                Err(_) => anyhow::bail!(
                    "no reply at {TARGET} ({}), also silent at {old_baud} — ECU unresponsive; power-cycle it",
                    last_err.map(|e| format!("{e}")).unwrap_or_default()
                ),
            }
        }
        _ => {
            eprintln!("unknown command: {command}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // GUI when there are no arguments, or when a GUI-only flag is present
    // (CLI commands are command-first: `dde3-toolkit <command> [options]`).
    const GUI_FLAGS: &[&str] = &[
        "--gui",
        "--demo",
        "--autoconnect",
        "--auto-live",
        "--auto-read",
        "--auto-cal",
        "--auto-log",
        "--auto-write-cal",
        "--auto-write",
        "--fast-write",
        "--show-vars",
        "--tab",
        "--width",
        "--height",
    ];
    if args.len() == 1 || args.iter().any(|a| GUI_FLAGS.contains(&a.as_str())) {
        let demo = args.iter().any(|a| a == "--demo");
        let autoconnect = args.iter().any(|a| a == "--autoconnect");
        let auto_live = args.iter().any(|a| a == "--auto-live");
        let auto_read = args.iter().any(|a| a == "--auto-read");
        let auto_cal = args.iter().any(|a| a == "--auto-cal");
        let auto_log = args
            .iter()
            .position(|a| a == "--auto-log")
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from);
        let image = args
            .iter()
            .position(|a| a == "--image")
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from);
        let auto_write_cal = args.iter().any(|a| a == "--auto-write-cal");
        let auto_write = args.iter().any(|a| a == "--auto-write");
        let fast_write = args.iter().any(|a| a == "--fast-write");
        let show_vars = args.iter().any(|a| a == "--show-vars");
        let port = args
            .iter()
            .position(|a| a == "--port")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let tab = args
            .iter()
            .position(|a| a == "--tab")
            .and_then(|i| args.get(i + 1))
            .map(|s| app::Tab::parse(s))
            .unwrap_or(app::Tab::Overview);
        let width = args
            .iter()
            .position(|a| a == "--width")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1180.0);
        let height = args
            .iter()
            .position(|a| a == "--height")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(760.0);
        let native_options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([width, height])
                .with_min_inner_size([940.0, 620.0])
                .with_title("DDE3 Toolkit"),
            ..Default::default()
        };
        let _ = eframe::run_native(
            "DDE3 Toolkit",
            native_options,
            Box::new(move |cc| {
                Ok(Box::new(app::App::new(
                    cc,
                    demo,
                    tab,
                    autoconnect,
                    auto_live,
                    auto_read,
                    auto_cal,
                    auto_log,
                    show_vars,
                    port,
                    image,
                    auto_write_cal,
                    auto_write,
                    fast_write,
                )))
            }),
        );
        return;
    }
    if let Err(e) = run() {
        eprintln!("ERROR: {e:#}");
        std::process::exit(1);
    }
}
