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
mod checksum;
mod dtc;
mod protocol;
mod sim;

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::checksum::{correct, verify, IMAGE_SIZE};
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
    eprintln!("  fault-clear                Clear fault memory (K-line BAP 12 09 07 0E 42 41 50 00)");
    eprintln!("  dtc                        Read DS2 fault memory with texts and freeze frames");
    eprintln!("  ds2-clear                  Clear DS2 fault memory (12 04 05)");
    eprintln!("  bench                      Headless run: identify + optional read/write,");
    eprintln!("                             progress and timing on stderr, useful exit status");
    eprintln!("    [--info|--read|--read-cal|--write|--write-cal]");
    eprintln!("    [--image <path>] [--out <path>]");
    eprintln!("  checksum <image.bin>       Verify and optionally correct flash checksums");
    eprintln!("  uprog-ein                  Enter programming mode (12 04 0D)");
    eprintln!("  diagnose-ende              Leave session / end diagnostics (12 04 9F)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --port <path>              Sender serial port (default /dev/ttyUSB1, FT232R)");
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
    let data = std::fs::read(path)
        .with_context(|| format!("cannot read image file {path}"))?;
    if data.len() != expected {
        anyhow::bail!("image {path} is {} bytes, expected {} ({})", data.len(), expected, path);
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
    let mut port = "/dev/ttyUSB1".to_string();
    let mut baud: u32 = 9600;
    let mut data_delay_ms: u64 = 3;
    let echo: bool = true;
    let mut sim: bool = false;
    let mut bench_image: Option<String> = None;
    let mut bench_out: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                port = args.get(i).cloned().context("--port requires a value")?;
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
            "--image" => {
                i += 1;
                bench_image = Some(
                    args.get(i)
                        .cloned()
                        .context("--image requires a value")?,
                );
            }
            "--out" => {
                i += 1;
                bench_out = Some(
                    args.get(i)
                        .cloned()
                        .context("--out requires a value")?,
                );
            }
            _ => {}
        }
        i += 1;
    }

    match command.as_str() {
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
                    anyhow::bail!("unexpected flash read reply at 0x{addr:06X}: {}", protocol::hex(&r));
                }
                let data_start = 3;
                if r.len() < data_start + count as usize {
                    anyhow::bail!("short flash read reply at 0x{addr:06X}: {}", protocol::hex(&r));
                }
                image.extend_from_slice(&r[data_start..data_start + count as usize]);
                addr += count as u32;
                done += count as usize;
                println!("read 0x{addr:06X} / 0x{:06X} ({} bytes)", IMAGE_SIZE, done);
            }
            std::fs::write(&out, &image)
                .with_context(|| format!("cannot write {out}"))?;
            println!("wrote {} bytes to {out}", image.len());
            Ok(())
        }
        "read-cal" => {
            let out = args.get(2).context("read-cal requires <out.bin>").cloned()?;
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
                    anyhow::bail!("unexpected flash read reply at 0x{addr:06X}: {}", protocol::hex(&r));
                }
                let data_start = 3;
                if r.len() < data_start + count as usize {
                    anyhow::bail!("short flash read reply at 0x{addr:06X}: {}", protocol::hex(&r));
                }
                image.extend_from_slice(&r[data_start..data_start + count as usize]);
                addr += count as u32;
            }
            std::fs::write(&out, &image)
                .with_context(|| format!("cannot write {out}"))?;
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
            let cal_path = args.get(2).context("write-cal requires <cal.bin>").cloned()?;
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
            let image_path = args.get(2).context("checksum requires <image.bin>").cloned()?;
            let image = read_image(&image_path, IMAGE_SIZE)?;
            let results = verify(&image);
            for r in &results {
                println!(
                    "checksum {:<16} {:08X} {}",
                    r.name,
                    r.value,
                    if r.ok { "OK" } else { "BAD" }
                );
            }
            let all_ok = results.iter().all(|r| r.ok);
            if !all_ok {
                eprintln!("WARNING: DDE3 checksum algorithm is UNVERIFIED — do not flash corrected images without hardware validation");
                let fixed = correct(&image);
                std::fs::write(&image_path, &fixed)
                    .with_context(|| format!("cannot write corrected image {image_path}"))?;
                println!("checksums corrected and written to {image_path}");
            } else {
                println!("all checksums OK");
            }
            if all_ok {
                Ok(())
            } else {
                anyhow::bail!("checksums were bad and have been corrected (UNVERIFIED algorithm)");
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
        "diagnose-ende" => {
            let mut k = Kline::open(&port, baud, echo)?;
            k.set_data_delay(data_delay_ms);
            if sim {
                k.set_sim(crate::sim::SimEcu::new());
            }
            let _ = k.request(&crate::protocol::frame_diagnose_ende(), Duration::from_secs(3))?;
            println!("DIAGNOSE_ENDE sent");
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
    // No arguments → launch the egui GUI.
    if args.len() == 1 || args.iter().any(|a| a == "--gui") {
        let native_options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size([940.0, 620.0])
                .with_title("DDE3 Toolkit"),
            ..Default::default()
        };
        let _ = eframe::run_native(
            "DDE3 Toolkit",
            native_options,
            Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
        );
        return;
    }
    if let Err(e) = run() {
        eprintln!("ERROR: {e:#}");
        std::process::exit(1);
    }
}
