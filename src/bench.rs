//! Headless bench runner for repeatable hardware tests.
//!
//! Uses the same protocol and checksum code as the CLI/GUI, prints progress
//! to stderr, and exits with a useful status:
//!
//!   PROGRESS <done>/<total>   — every ~4 KB of a read / every 8 KB of a write
//!   SAVED <path> <n> bytes    — read finished
//!   WRITE_START / WRITE_DONE  — write finished
//!   TRACE <line>              — protocol log tail on failure
//!   ELAPSED <s>               — total wall time
//!
//! DDE3 specifics vs. dde4-toolkit:
//!   - image is 256 KB (0x40000), calibration area is 0x38000-0x3FFFF (32 KB)
//!   - no fast-write mode: DDE3 K-line is 9600 8E1 (baud is passed through,
//!     `write_full` currently ignores it)
//!   - checksums are TDI V3.1 (see `checksum.rs`); correction of a bare 32 KB
//!     calibration slice works via the marker-less block-id path

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::checksum::{self, CAL_LEN, CAL_START};
use crate::protocol::{hex, ident_valid, Kline, MW_BATCH_SIZE, IMAGE_SIZE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Info,
    Read,
    ReadCal,
    Write,
    WriteCal,
    Live,
}

pub struct BenchArgs {
    pub port: String,
    pub image: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub mode: Mode,
    pub baud: u32,
    pub data_delay: u64,
    pub sim: bool,
    /// Switch the DS2 link up via service 0x91 right after IDENT (needs the baud
    /// patch flashed). 38400 or 125000; the port reopens at the actual divisor.
    pub fast_baud: Option<u32>,
    /// For `Mode::Live`: how long to log before stopping (seconds). None = ~10 s.
    pub duration: Option<f64>,
}

pub fn run(args: BenchArgs) -> Result<()> {
    let started = Instant::now();
    let mut k = Kline::open(&args.port, args.baud)
        .with_context(|| format!("cannot open {}", args.port))?;
    k.set_data_delay(args.data_delay);
    if args.sim {
        k.set_sim(crate::sim::SimEcu::new());
    }
    let ident = k.identify().context("ECU did not answer A2")?;
    eprintln!(
        "IDENT {} ({})",
        hex(&ident),
        if ident_valid(&ident) {
            "OK"
        } else {
            "ERASED/CORRUPT - full write needed"
        }
    );

    // Optional fast-baud switch (service 0x91 — needs the baud patch flashed).
    let fast_active = if let Some(rate) = args.fast_baud {
        let reopen = k.switch_fast_baud(rate, args.baud)?;
        eprintln!("FASTBAUD {reopen} (revert by power cycle)");
        true
    } else {
        false
    };

    let result = (|| -> Result<()> {
        match args.mode {
            Mode::Info => Ok(()),
            Mode::Live => {
                // Default-on live set, polled in batched 0B 10 telegrams.
                let addrs: Vec<(u8, u8)> = crate::live::LIVE_VARS
                    .iter()
                    .filter(|v| v.default_on)
                    .map(|v| ((v.id >> 8) as u8, v.id as u8))
                    .collect();
                if addrs.is_empty() {
                    bail!("no default-on live variables");
                }
                if fast_active {
                    k.set_write_delay(0); // wire, not pacing, dominates at fast baud
                }
                let secs = args.duration.unwrap_or(10.0);
                let secs = if secs.is_finite() && secs > 0.0 { secs } else { 10.0 };
                let mut cycles = 0u64;
                // Anchor the window at the loop start (not fn entry), so port-open +
                // IDENT + baud-switch latency does not eat into a short --duration.
                let loop_start = Instant::now();
                let deadline = loop_start + Duration::from_secs_f64(secs);
                while Instant::now() < deadline {
                    for chunk in addrs.chunks(MW_BATCH_SIZE) {
                        let vals = k.live_values(chunk, false)?;
                        cycles += 1;
                        if cycles.is_multiple_of(10) {
                            eprintln!("LIVE n={} first={:?}", vals.len(), vals.first());
                        }
                    }
                }
                let hz = cycles as f64 / loop_start.elapsed().as_secs_f64();
                eprintln!("LIVE_DONE cycles={cycles} rate={hz:.1}Hz ids={}", addrs.len());
                Ok(())
            }
            Mode::Read => {
                let out = args.out.clone().context("read needs --out <path>")?;
                let mut progress = |done: usize, total: usize| {
                    if done % 0x4000 < 0x80 {
                        eprintln!("PROGRESS {done}/{total}");
                    }
                };
                let data = k.read_full(&mut progress)?;
                std::fs::write(&out, &data)
                    .with_context(|| format!("cannot save {}", out.display()))?;
                eprintln!("SAVED {} {} bytes", out.display(), data.len());
                Ok(())
            }
            Mode::ReadCal => {
                let out = args
                    .out
                    .clone()
                    .context("calibration read needs --out <path>")?;
                let mut progress = |done: usize, total: usize| {
                    if done % 0x2000 < 0x80 {
                        eprintln!("PROGRESS {done}/{total}");
                    }
                };
                let data = k.read_range(CAL_START, CAL_LEN, &mut progress)?;
                std::fs::write(&out, &data)
                    .with_context(|| format!("cannot save {}", out.display()))?;
                eprintln!("SAVED {} {} bytes", out.display(), data.len());
                Ok(())
            }
            Mode::Write | Mode::WriteCal => {
                let path = args.image.clone().context("write needs --image <path>")?;
                let raw = std::fs::read(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                let image = prepare_image(&raw, args.mode)?;
                eprintln!(
                    "WRITE_START mode={:?} baud={} delay={}ms",
                    args.mode, args.baud, args.data_delay
                );
                let mut progress = |done: usize, total: usize| {
                    if done % 0x8000 < 0x80 {
                        eprintln!("PROGRESS {done}/{total}");
                    }
                };
                match args.mode {
                    Mode::Write => {
                        k.write_full(&image, args.baud, args.data_delay, &mut progress)?
                    }
                    Mode::WriteCal => {
                        k.write_calibration(&image, args.baud, args.data_delay, &mut progress)?
                    }
                    _ => unreachable!(),
                }
                eprintln!("WRITE_DONE");
                Ok(())
            }
        }
    })();

    // After a fast-baud session the ECU stays at the new rate until a power
    // cycle, so the forced-9600 recover would misreport. Skip it then.
    let recovered = if fast_active { true } else { k.recover_link() };
    if result.is_err() {
        for line in k.take_logs().iter().rev().take(120).rev() {
            eprintln!("TRACE {line}");
        }
    }
    if fast_active && result.is_ok() {
        eprintln!("NOTE ECU remains at the fast baud until power cycle");
    } else if !recovered {
        if result.is_ok() {
            eprintln!("NOTE ECU is still booting; wait about 30-60 s before reconnecting");
        } else {
            eprintln!("WARN link did not return to 9600/A2");
        }
    }
    eprintln!("ELAPSED {:.1}s", started.elapsed().as_secs_f32());
    result
}

/// Validate and prepare the image for a write.
fn prepare_image(raw: &[u8], mode: Mode) -> Result<Vec<u8>> {
    match mode {
        Mode::Write => {
            if raw.len() != IMAGE_SIZE {
                bail!("full write needs a 256 KB image, got {} bytes", raw.len());
            }
            let fixed = checksum::correct(raw);
            let bad: Vec<_> = checksum::verify(&fixed)
                .iter()
                .filter(|r| !r.ok)
                .map(|r| r.name)
                .collect();
            if !bad.is_empty() {
                bail!("image checksum still bad in {bad:?}");
            }
            Ok(fixed)
        }
        Mode::WriteCal => match raw.len() {
            // Full image: correct everything, hand over the calibration slice.
            IMAGE_SIZE => {
                let fixed = checksum::correct(raw);
                Ok(fixed[CAL_START..CAL_START + CAL_LEN].to_vec())
            }
            // Bare 32 KB calibration slice: pad into a full image so the
            // block-id (marker-less) checksum path can find the block, then
            // correct it.
            CAL_LEN => {
                let mut full = vec![0xFFu8; IMAGE_SIZE];
                full[CAL_START..CAL_START + CAL_LEN].copy_from_slice(raw);
                if checksum::verify(&full).is_empty() {
                    bail!("no EDC15M TDI V3.1 checksum block found in calibration image");
                }
                let fixed = checksum::correct(&full);
                Ok(fixed[CAL_START..CAL_START + CAL_LEN].to_vec())
            }
            other => bail!("calibration write needs 256 KB or 32 KB, got {other} bytes"),
        },
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_image(seed: u8) -> Vec<u8> {
        let mut img = vec![0u8; IMAGE_SIZE];
        for (i, b) in img.iter_mut().enumerate() {
            *b = seed.wrapping_add((i % 251) as u8);
        }
        img
    }

    #[test]
    fn write_image_is_full_and_checked() {
        let img = synthetic_image(0x11);
        let fixed = prepare_image(&img, Mode::Write).expect("correction succeeds");
        assert_eq!(fixed.len(), IMAGE_SIZE);
        let bad: Vec<_> = checksum::verify(&fixed)
            .iter()
            .filter(|r| !r.ok)
            .map(|r| r.name)
            .collect();
        assert!(bad.is_empty(), "corrected image must verify: {bad:?}");
    }

    #[test]
    fn write_image_rejects_wrong_size() {
        let err = prepare_image(&[0u8; 1024], Mode::Write).unwrap_err();
        assert!(err.to_string().contains("256 KB"));
    }

    #[test]
    fn cal_image_from_full_is_32kb() {
        let img = synthetic_image(0x22);
        let cal = prepare_image(&img, Mode::WriteCal).expect("full image cal slice");
        assert_eq!(cal.len(), CAL_LEN);
    }

    #[test]
    fn cal_image_rejects_wrong_size() {
        let err = prepare_image(&[0u8; 4096], Mode::WriteCal).unwrap_err();
        assert!(err.to_string().contains("256 KB or 32 KB"));
    }
}
