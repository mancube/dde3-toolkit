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
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::checksum::{self, CAL_LEN, CAL_START};
use crate::protocol::{ident_valid, hex, Kline, IMAGE_SIZE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Info,
    Read,
    ReadCal,
    Write,
    WriteCal,
}

pub struct BenchArgs {
    pub port: String,
    pub image: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub mode: Mode,
    pub baud: u32,
    pub data_delay: u64,
    pub sim: bool,
}

pub fn run(args: BenchArgs) -> Result<()> {
    let started = Instant::now();
    let mut k = Kline::open(&args.port, args.baud, true)
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

    let result = (|| -> Result<()> {
        match args.mode {
            Mode::Info => Ok(()),
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
                    Mode::Write => k.write_full(&image, args.baud, args.data_delay, &mut progress)?,
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

    let recovered = k.recover_link();
    if result.is_err() {
        for line in k.take_logs().iter().rev().take(120).rev() {
            eprintln!("TRACE {line}");
        }
    }
    if !recovered {
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
                .into_iter()
                .filter(|r| checksum::CORRECTABLE.contains(&r.name) && !r.ok)
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
            // correct just the calibration block.
            CAL_LEN => {
                let mut full = vec![0xFFu8; IMAGE_SIZE];
                full[CAL_START..CAL_START + CAL_LEN].copy_from_slice(raw);
                let fixed = checksum::correct_block(&full, "calibration")
                    .context("cannot correct the calibration checksum")?;
                Ok(fixed[CAL_START..CAL_START + CAL_LEN].to_vec())
            }
            other => bail!(
                "calibration write needs 256 KB or 32 KB, got {other} bytes"
            ),
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
            .into_iter()
            .filter(|r| checksum::CORRECTABLE.contains(&r.name) && !r.ok)
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
