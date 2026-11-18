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
mod patches;
mod protocol;
mod respond;
mod sim;

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::checksum::{correct, verify, CAL_LEN, CAL_START, IMAGE_SIZE};
use crate::protocol::{Kline, FLASH_WRITE_START};

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn load_icon() -> eframe::egui::IconData {
    let image = image::load_from_memory(ICON_PNG)
        .expect("assets/icon.png is a valid PNG")
        .to_rgba8();
    let (width, height) = image.dimensions();
    eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn print_usage() {
    eprintln!("dde3-toolkit v{}", env!("CARGO_PKG_VERSION"));
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
    eprintln!("  respond                FAKE ECU: full duplex on ONE port — it answers every");
    eprintln!("                             received frame like an EDC15 would (built-in sim:");
    eprintln!("                             IDENT, STATUS, SEED/KEY, flash, DTC, live data).");
    eprintln!("                             Use it as the ECU stand-in while a flashing tool");
    eprintln!("                             drives the same K node; own replies are logged as ECHO.");
    eprintln!("    [--out <file>] [--duration <secs>] [--data-delay <ms>]");
    eprintln!("    [--reply-delay <ms>] [--dump <image.bin>]");
    eprintln!("    [--flash <image.bin>]");
    eprintln!("                             (default port /dev/ttyUSB0; --dump writes the");
    eprintln!("                             reconstructed flash image the peer programmed —");
    eprintln!("                             the 192 KB 0x10000..0x40000 test write is padded");
    eprintln!("                             FF to a full 256 KB image; --flash serves a real");
    eprintln!("                             256 KB dump from the fake ECU: FLASH_LESEN + AIF");
    eprintln!("                             read return the file's bytes)");
    eprintln!("  bench                      Headless run: identify + optional read/write,");
    eprintln!("                             progress and timing on stderr, useful exit status");
    eprintln!("    [--info|--read|--read-cal|--write|--write-cal|--live]");
    eprintln!("    [--image <path>] [--out <path>] [--fast-baud 38400|125000] [--duration <s>]");
    eprintln!("    [--fast-9600] (--live only: back-to-back polling + 1 ms/byte pacing;");
    eprintln!("    default is the original 55 ms / 3 ms timing; --fast-baud needs the patch)");
    eprintln!("  checksum <image.bin>       Verify and optionally correct flash checksums");
    eprintln!("  patches [list|show|apply|revert|verify|correct] [name]");
    eprintln!("                             Firmware patch manifests from ./patches (baud 0x91,");
    eprintln!("                             EWS delete). --image <in.bin> [--out <out.bin>]");
    eprintln!("                             apply/revert corrects every flash checksum, program");
    eprintln!("                             chain included; `correct --image` just fixes checksums");
    eprintln!("                             on an already-patched read. Nothing is sent to the ECU.");
    eprintln!("  uprog-ein                  Enter programming mode (12 04 0D)");
    eprintln!("  break-probe              Enter the break-entered service protocol, send one
                             probe byte, dump the response (--fast for 38.4k)");
    eprintln!("  diagnose-ende              Leave session / end diagnostics (12 04 9F)");
    eprintln!("  baud [--rate 38400|125000] Switch the DS2 link up via service 0x91 (needs the baud patch)");
    eprintln!("                             (requires the 0x91 flash patch: patches/e46_baud<rate>.bin,");
    eprintln!("                             write it first; see patches/README.md)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --port <path>              Serial port (default /dev/ttyUSB1 FT232R sender;");
    eprintln!("                             capture defaults to /dev/ttyUSB0 passive tap)");
    eprintln!("  --duration <secs>          Capture duration in seconds (capture only;");
    eprintln!("                             default: run until Ctrl-C)");
    eprintln!("  --baud <rate>              Baud rate (default 9600)");
    eprintln!("  --data-delay <ms>          Per-byte delay for control frames (default 3)");
    eprintln!("  --experimental           EXPERIMENTAL: 192-byte flash blocks instead of the
                             known-safe 64 B writes / 128 B reads (closer to the
                             ECU's acceptance limit of 246 B/block). Not car-verified.
                             Writes additionally require typing WRITE192 (not WRITE).");
    eprintln!("  --sim                      Run against the built-in ECU simulator");
    eprintln!("                             (frames still go over the wire; the ECU reply");
    eprintln!("                             is simulated because the monitor adapter is passive)");
    eprintln!();
    eprintln!("Safety: A write requires typing WRITE to confirm (WRITE192 in");
    eprintln!("--experimental mode). Flash checksums are");
    eprintln!("verified and corrected on write (TDI V3.1 + program chain), byte-for-byte");
    eprintln!("parity with the verified edc15m-checksum-tool and progsum.py.");
}

fn parse_hex_u8(s: &str) -> Result<u8> {
    u8::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .with_context(|| format!("invalid byte value: {s}"))
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

/// Write gate: type WRITE — or WRITE192 in `--experimental` mode, where the
/// stricter word is the second gate on top of the flag itself.
fn confirm_write(experimental: bool) -> Result<()> {
    let word = if experimental { "WRITE192" } else { "WRITE" };
    if experimental {
        eprintln!("EXPERIMENTAL: 192-byte flash blocks (not car-verified — bench-test first).");
    }
    print!("Type {word} to confirm flash write: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if line.trim() != word {
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
    let mut data_delay_set = false;
    // `respond --reply-delay`: pause (ms) between the peer's last byte and the
    // first reply byte of the fake ECU (default 5 ms, ECU-like latency).
    let mut reply_delay_ms: u64 = 5;
    let mut sim: bool = false;
    let mut fast: bool = false;
    // Experimental 192-byte flash blocks (read + write, full + cal). Writes
    // additionally require the WRITE192 confirmation word.
    let mut experimental: bool = false;
    // Target DS2 rate for the `baud` command (the 0x91 patch). 38400 or 125000.
    let mut target_rate: u32 = 38_400;
    // `bench --fast-baud <rate>`: switch up right after IDENT (needs the patch).
    let mut fast_baud: Option<u32> = None;
    // `bench --live --fast-9600`: the faster 9600 timing (back-to-back,
    // 1 ms/byte) instead of the default WinKFP timing (55 ms gap, 3 ms/byte).
    let mut fast9600: bool = false;
    let mut bench_image: Option<String> = None;
    let mut bench_out: Option<String> = None;
    // `respond --dump <file>`: at run end, write the reconstructed flash
    // image (everything the peer programmed) to this file.
    let mut bench_dump: Option<String> = None;
    // `respond --flash <file>`: serve this real flash dump from the fake ECU.
    let mut respond_flash: Option<String> = None;

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
                data_delay_set = true;
            }
            "--rate" => {
                i += 1;
                target_rate = args
                    .get(i)
                    .context("--rate requires a value")?
                    .parse::<u32>()
                    .with_context(|| format!("invalid rate: {}", args[i]))?;
            }
            "--fast-baud" => {
                i += 1;
                fast_baud = Some(
                    args.get(i)
                        .context("--fast-baud requires a value")?
                        .parse::<u32>()
                        .with_context(|| format!("invalid fast-baud rate: {}", args[i]))?,
                );
            }
            "--sim" => sim = true,
            "--fast" => fast = true,
            "--experimental" => experimental = true,
            "--fast-9600" => fast9600 = true,
            "--reply-delay" => {
                i += 1;
                reply_delay_ms = args
                    .get(i)
                    .context("--reply-delay requires a value")?
                    .parse::<u64>()
                    .with_context(|| format!("invalid reply delay: {}", args[i]))?;
            },
            "--image" => {
                i += 1;
                bench_image = Some(args.get(i).cloned().context("--image requires a value")?);
            }
            "--out" => {
                i += 1;
                bench_out = Some(args.get(i).cloned().context("--out requires a value")?);
            }
            "--dump" => {
                i += 1;
                bench_dump = Some(args.get(i).cloned().context("--dump requires a value")?);
            }
            "--flash" => {
                i += 1;
                respond_flash = Some(args.get(i).cloned().context("--flash requires a value")?);
            }
            _ => {}
        }
        i += 1;
    }

    // The fake ECU transmits back-to-back like the real one (the 3 ms/byte
    // WinKFP pacing is a master-side habit) unless the user set it explicitly.
    if command == "respond" && !data_delay_set {
        data_delay_ms = 0;
    }

    // Resolve the port: explicit --port wins; otherwise the sender for the
    // driver commands and the passive tap for `capture` (read-only).
    let port: String = port.unwrap_or_else(|| {
        if command == "capture" || command == "respond" {
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
        "respond" => {
            let opts = respond::RespondOptions {
                port,
                baud,
                out: bench_out.clone().map(PathBuf::from),
                duration: duration.map(Duration::from_secs_f64),
                data_delay_ms,
                reply_delay_ms,
                dump: bench_dump.clone().map(PathBuf::from),
                flash: respond_flash.map(PathBuf::from),
            };
            respond::run(&opts)?;
            Ok(())
        }
        "ident" => {
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let r = k.identify()?;
            match protocol::parse_ident(&r) {
                Some(i) => {
                    println!("IDENT string: {}", String::from_utf8_lossy(&r));
                    println!("  software:  {}", i.software);
                    println!("  ECU code:  {}", protocol::format_pair(&i.ecu_code));
                    println!("  SW date:   {} (week/year)", protocol::format_pair(&i.date));
                    println!("  HW number: {}", i.hw_number);
                    println!("  G code:    {}", i.g_code);
                }
                None => println!("IDENT reply: {}", protocol::hex(&r)),
            }
            Ok(())
        }
        "read" => {
            let out = args.get(2).context("read requires <out.bin>").cloned()?;
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            k.set_experimental_blocks(experimental);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            // Same code path as the GUI's full-flash read, including its
            // single retry with 20 ms silence on a non-link error.
            let mut progress = |done: usize, total: usize| {
                println!("read 0x{done:06X} / 0x{total:06X}");
            };
            let image = k.read_full(&mut progress)?;
            std::fs::write(&out, &image).with_context(|| format!("cannot write {out}"))?;
            println!("wrote {} bytes to {out}", image.len());
            Ok(())
        }
        "read-cal" => {
            let out = args
                .get(2)
                .context("read-cal requires <out.bin>")
                .cloned()?;
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            k.set_experimental_blocks(experimental);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            // Same code path as the GUI's calibration read.
            let mut progress = |done: usize, total: usize| {
                println!("read cal 0x{done:05X} / 0x{total:05X}");
            };
            let image = k.read_range(CAL_START, CAL_LEN, &mut progress)?;
            std::fs::write(&out, &image).with_context(|| format!("cannot write {out}"))?;
            println!("wrote {} bytes to {out}", image.len());
            Ok(())
        }
        "write" => {
            let image_path = args.get(2).context("write requires <image.bin>").cloned()?;
            let image = read_image(&image_path, IMAGE_SIZE)?;
            confirm_write(experimental)?;
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            k.set_experimental_blocks(experimental);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            // Same erase → write → end sequence as the GUI, including the
            // post-erase settle delay and the per-write-block byte delay.
            let mut progress = |done: usize, total: usize| {
                println!("wrote 0x{done:06X} / 0x{total:06X}");
            };
            k.write_full(&image, baud, data_delay_ms, &mut progress)?;
            println!("write complete");
            if sim {
                println!("verifying read-back...");
                let rb = k.read_full(&mut |_d, _t| {})?;
                // The full write only programs [FLASH_WRITE_START, IMAGE_SIZE)
                // (0x10000..0x40000); compare the written region, not the bytes
                // below 0x10000 that were never touched.
                if rb.len() == image.len() && rb[FLASH_WRITE_START..] == image[FLASH_WRITE_START..] {
                    println!(
                        "read-back verified: bytes 0x{:X}..0x{:X} of the image match the written region",
                        FLASH_WRITE_START,
                        image.len()
                    );
                } else {
                    let m = rb[FLASH_WRITE_START..]
                        .iter()
                        .zip(&image[FLASH_WRITE_START..])
                        .position(|(a, b)| a != b)
                        .unwrap_or(0);
                    anyhow::bail!("read-back mismatch at offset 0x{:X}", FLASH_WRITE_START + m);
                }
            }
            Ok(())
        }
        "write-cal" => {
            let cal_path = args
                .get(2)
                .context("write-cal requires <cal.bin>")
                .cloned()?;
            let cal = read_image(&cal_path, CAL_LEN)?;
            confirm_write(experimental)?;
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            k.set_experimental_blocks(experimental);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            // Same sequence as the GUI's calibration write.
            let mut progress = |done: usize, total: usize| {
                println!("wrote cal 0x{done:05X} / 0x{total:05X}");
            };
            k.write_calibration(&cal, baud, data_delay_ms, &mut progress)?;
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
            let mut k = Kline::open(&port, baud)?;
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
            let mut k = Kline::open(&port, baud)?;
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
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let dtcs = match k.read_dtcs() {
                Ok(dtcs) => dtcs,
                // Same trace dump as `bench`: the wire log is the only evidence
                // of what the ECU actually replied on a failed fault read.
                Err(e) => {
                    for line in k.take_logs().iter().rev().take(120).rev() {
                        eprintln!("TRACE {line}");
                    }
                    return Err(e);
                }
            };
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
            let mut k = Kline::open(&port, baud)?;
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
                    "--live" => mode = bench::Mode::Live,
                    "--info" => mode = bench::Mode::Info,
                    "--experimental" => experimental = true,
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
                fast_baud,
                duration,
                fast9600,
                experimental,
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
                    "checksum {:<14} 0x{:05X} stored {:08X} computed {:08X} {}",
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
        "patches" => patches_cmd(&args, bench_image, bench_out),
        "uprog-ein" => {
            let mut k = Kline::open(&port, baud)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let _ = k.request(&crate::protocol::frame_uprog_ein(), Duration::from_secs(3))?;
            println!("UPROG_EIN sent");
            Ok(())
        }
        "break-probe" => {
            let mut k = Kline::open(&port, baud)?;
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
            let mut k = Kline::open(&port, baud)?;
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
            // `--rate` selects which 0x91 patch image is flashed:
            //   38400  -> S0BG 0x0C (e46_baud38400.bin)
            //   125000 -> S0BG 0x03, ECU actually runs 124800 (e46_baud125000.bin)
            // The fixed-rate patch ignores the frame's baud field, but we send the
            // matching 24-bit value for wire-compat with a field-honoring (V2) patch.
            // The port must be REOPENED at the ECU's actual divisor, not the nominal.
            let old_baud = baud;
            let mut k = Kline::open(&port, old_baud)?;
            k.set_data_delay(data_delay_ms);
            println!("sending 0x91 @ {old_baud}: target {target_rate}");
            let reopen = k.switch_fast_baud(target_rate, old_baud)?;
            let r = k.request(&crate::protocol::frame_ident(), Duration::from_millis(500))?;
            println!("IDENT @ {reopen}: {}", protocol::hex(&r));
            println!("DS2 link now runs at {reopen} (revert to {old_baud} by power cycle)");
            Ok(())
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
    // GUI when there are no arguments, or when the first argument is a flag
    // (CLI commands are command-first: `dde3-toolkit <command> [options]`).
    // Shared flags like --experimental work in both worlds: `dde3-toolkit
    // write x.bin --experimental` is a CLI write, `dde3-toolkit --experimental`
    // launches the GUI with the experimental mode pre-enabled.
    if args.len() == 1 || args[1].starts_with('-') {
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
        // Program-wide experimental 192-byte block mode (GUI LINK toggle).
        let experimental = args.iter().any(|a| a == "--experimental");
        // GUI fast-baud target (takes effect when the GUI is launched via another
        // flag, e.g. --autoconnect --auto-live); the Live tab also has a button.
        let gui_fast_baud = args
            .iter()
            .position(|a| a == "--fast-baud")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u32>().ok());
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
                .with_title("DDE3 Toolkit")
                .with_app_id("dde3-toolkit")
                .with_icon(load_icon()),
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
                    gui_fast_baud,
                    experimental,
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

// ---------------------------------------------------------------------------
// `patches` subcommand — firmware patch manifests (baud 0x91, EWS delete)
// ---------------------------------------------------------------------------

/// Positional words after `patches`, skipping the value-taking global flags.
fn patch_positionals(args: &[String]) -> Vec<String> {
    let mut pos = Vec::new();
    let mut i = 2; // binary, "patches"
    while i < args.len() {
        match args[i].as_str() {
            "--image" | "--out" | "--port" | "--baud" | "--data-delay" | "--duration"
            | "--rate" | "--fast-baud" => i += 2,
            s if s.starts_with("--") => i += 1,
            s => {
                pos.push(s.to_string());
                i += 1;
            }
        }
    }
    pos
}

/// Resolve a patch by exact name, unique case-insensitive substring, or the
/// 1-based index printed by `patches list`.
fn find_patch<'a>(all: &'a [patches::Patch], needle: Option<&String>) -> Result<&'a patches::Patch> {
    let Some(needle) = needle else {
        anyhow::bail!("name a patch (see `dde3-toolkit patches list`)");
    };
    if let Some(p) = all.iter().find(|p| p.name == *needle) {
        return Ok(p);
    }
    if let Ok(idx) = needle.parse::<usize>() {
        if let Some(p) = all.get(idx.wrapping_sub(1)) {
            return Ok(p);
        }
    }
    let lower = needle.to_ascii_lowercase();
    let hits: Vec<&patches::Patch> = all
        .iter()
        .filter(|p| p.name.to_ascii_lowercase().contains(&lower))
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => anyhow::bail!(
            "no patch matches {needle:?}; available: {}",
            all.iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => anyhow::bail!(
            "{needle:?} is ambiguous; matches: {}",
            hits.iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Print the checksum report for an image; returns whether every check passed.
fn report_checksums(image: &[u8]) -> bool {
    let checks = verify(image);
    if checks.is_empty() {
        println!("checksums: no EDC15M checksum block found in this image");
        return false;
    }
    for c in &checks {
        println!(
            "checksum {:<14} 0x{:05X} stored {:08X} computed {:08X} {}",
            c.name,
            c.pos,
            c.stored,
            c.computed,
            if c.ok { "OK" } else { "BAD" }
        );
    }
    checks.iter().all(|c| c.ok)
}

fn patches_cmd(args: &[String], image: Option<String>, out: Option<String>) -> Result<()> {
    let pos = patch_positionals(args);
    let action = pos.first().map(String::as_str).unwrap_or("list");
    let (all, warnings) = patches::load_all();
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if all.is_empty() {
        anyhow::bail!("no patch manifests found (expected ./patches/*.json)");
    }
    let loaded = match &image {
        Some(path) => Some(read_image(path, IMAGE_SIZE)?),
        None => None,
    };

    match action {
        "list" => {
            for (i, p) in all.iter().enumerate() {
                let state = loaded.as_ref().map(|img| p.state(img));
                println!(
                    "{:>2}. {:<28} v{}  {:<10} {}",
                    i + 1,
                    p.name,
                    p.version,
                    state.as_ref().map(|s| s.label()).unwrap_or("-"),
                    p.description.lines().next().unwrap_or("")
                );
                if let Some(detail) = state.as_ref().and_then(|s| s.detail()) {
                    println!("    {detail}");
                }
            }
            Ok(())
        }
        "show" => {
            let p = find_patch(&all, pos.get(1))?;
            println!("{}  (v{})", p.name, p.version);
            println!("source:  {}", p.source.label());
            if let Some(o) = &p.output {
                println!("output:  {o}");
            }
            println!("description: {}", p.description);
            if !p.verified.is_empty() {
                println!("verified: {}", p.verified);
            }
            println!(
                "hardware tested: {}",
                if p.hardware_tested { "yes" } else { "no" }
            );
            if let Some(h) = &p.target_sha256 {
                println!("reference read sha256: {h}  (informational; any read with matching edits works)");
            }
            if !p.checksum_note.is_empty() {
                println!("checksums: {}", p.checksum_note);
            }
            println!(
                "edits ({} ranges, {} bytes) — checksum fields are recomputed on apply:",
                p.semantic_changes().len(),
                p.changed_bytes()
            );
            for c in &p.changes {
                println!(
                    "  0x{:05X}  {:<8} {:<16} -> {}  {}",
                    c.offset,
                    format!("{} B", c.after.len()),
                    hex_prefix(&c.before),
                    hex_prefix(&c.after),
                    if c.is_checksum() { "(checksum)" } else { "" }
                );
            }
            Ok(())
        }
        "correct" => {
            let in_path = image
                .as_deref()
                .context("--image <in.bin> is required")?;
            let img = read_image(in_path, IMAGE_SIZE)?;
            let fixed = correct(&img);
            let changed = fixed != img;
            let out_path = out.unwrap_or_else(|| {
                let stem = in_path.trim_end_matches(".bin").trim_end_matches(".BIN");
                format!("{stem}.corrected.bin")
            });
            std::fs::write(&out_path, &fixed)
                .with_context(|| format!("cannot write {out_path}"))?;
            println!(
                "checksums {} -> {out_path}",
                if changed { "corrected" } else { "already valid" }
            );
            if !report_checksums(&fixed) {
                anyhow::bail!("{out_path} still has a bad checksum");
            }
            Ok(())
        }
        "apply" | "revert" => {
            let p = find_patch(&all, pos.get(1))?;
            let in_path = image
                .as_deref()
                .context("--image <in.bin> is required")?;
            let mut img = read_image(in_path, IMAGE_SIZE)?;
            if action == "apply" {
                p.apply(&mut img)?;
            } else {
                p.revert(&mut img)?;
            }
            let out_path = out.unwrap_or_else(|| {
                let stem = in_path.trim_end_matches(".bin").trim_end_matches(".BIN");
                format!("{stem}.{action}ed.bin")
            });
            std::fs::write(&out_path, &img)
                .with_context(|| format!("cannot write {out_path}"))?;
            println!("{action} {} -> {out_path}", p.name);
            let ok = report_checksums(&img);
            if !ok {
                anyhow::bail!("{out_path} still has a bad checksum");
            }
            println!("all checksums OK — review the image, then write it from the Write tab");
            Ok(())
        }
        "verify" => {
            let p = find_patch(&all, pos.get(1))?;
            let img = loaded.context("--image <in.bin> is required")?;
            let state = p.state(&img);
            println!("{}: {}", p.name, state.label());
            let ok = report_checksums(&img);
            match state {
                patches::PatchState::Applied if ok => Ok(()),
                patches::PatchState::Applied => anyhow::bail!("checksums are bad"),
                patches::PatchState::Available => anyhow::bail!("the patch is not applied"),
                patches::PatchState::Conflict(d) | patches::PatchState::Invalid(d) => {
                    anyhow::bail!("{d}")
                }
            }
        }
        other => anyhow::bail!(
            "unknown patches action {other:?}; use list | show <name> | apply <name> | revert <name> | verify <name> | correct"
        ),
    }
}

fn hex_prefix(bytes: &[u8]) -> String {
    const MAX: usize = 8;
    let mut s: String = bytes
        .iter()
        .take(MAX)
        .map(|b| format!("{b:02x}"))
        .collect();
    if bytes.len() > MAX {
        s.push('…');
    }
    s
}
