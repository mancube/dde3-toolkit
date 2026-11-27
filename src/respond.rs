//! Fake-ECU responder — answers K-line frames like an EDC15 would.
//!
//! Single-port full duplex: the adapter on `port` (default `/dev/ttyUSB0`)
//! BOTH receives the peer's frames (a flashing tool / another driver
//! adapter on the same K node) and transmits the simulated ECU replies.
//! The peer's frames are framed with the same [`crate::capture::Framer`]
//! as the passive capture (0x12 anchor, shortest-XOR terminator, verbatim
//! RAW runs), and every complete frame is fed to the built-in
//! [`crate::sim::SimEcu`] state machine — IDENT, STATUS, SEED/KEY, flash
//! read/erase/write, DTC memory, MW live data — whose reply is then
//! written to the same port, wire frame (payload + trailing XOR) with
//! optional per-byte pacing.
//!
//! The adapter's RX sees its own transmissions (bus echo). Each
//! transmitted reply is therefore matched against the next extracted
//! frame and logged as `ECHO` instead of being re-processed.
//!
//! Line format (same convention as `capture`, timestamp = seconds since
//! start, one line per event):
//!
//! ```text
//! [  12.345] RX   12 04 00 16   IDENT
//! [  12.400] TX   12 2E A0 37 37 38 36 …   IDENT-REPLY
//! [  12.500] ECHO 12 2E A0 37 37 38 36 …   (own reply, bus echo)
//! [  12.600] RAW  CF CF CF
//! ```

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::{ClearBuffer, DataBits, Parity, SerialPort, StopBits};

use crate::capture::{self, Event, Framer, STOP};
use crate::protocol::build_frame;
use crate::sim::SimEcu;

pub struct RespondOptions {
    pub port: String,
    pub baud: u32,
    /// Mirror all lines to this file (for `tail -f`).
    pub out: Option<std::path::PathBuf>,
    /// Stop after this long (`None` = until Ctrl-C).
    pub duration: Option<Duration>,
    /// Per-byte pacing of transmitted replies (0 = back-to-back, like the
    /// real ECU).
    pub data_delay_ms: u64,
    /// Pause between the peer's last byte and the first reply byte.
    pub reply_delay_ms: u64,
    /// Dump the reconstructed flash image (everything the peer wrote) to
    /// this file at the end of the run.
    pub dump: Option<std::path::PathBuf>,
    /// Serve this real flash dump from the fake ECU: FLASH_LESEN (06) and the
    /// AIF read (0x02) answer with the file's bytes. `None` = built-in
    /// deterministic pattern.
    pub flash: Option<std::path::PathBuf>,
    /// 0x91 baud patch: configure the fake ECU like the patched real ECU
    /// (the `e46_baud<rate>.bin` images). `Some(38400 | 125000)` — a `0x91`
    /// then switches the port to the patch's actual rate (125000 → 124800,
    /// the S0BG divisor) and the ACK goes out at the NEW rate, so `baud`
    /// and the fast-baud read/write path can be tested end-to-end.
    /// `None` = no patch (the 0x91 stub ACK stays, nothing switches).
    pub baud_patch: Option<u32>,
}

pub struct Summary {
    pub frames: u64,
    pub replies: u64,
    pub raw_bytes: u64,
    pub services: BTreeMap<String, u64>,
    pub elapsed: Duration,
}

impl Summary {
    pub fn render(&self) -> String {
        let mut s = format!(
            "respond summary: {}/{} frames answered, {} raw bytes, {:7.3} s\n",
            self.replies, self.frames, self.raw_bytes,
            self.elapsed.as_secs_f32()
        );
        for (name, n) in &self.services {
            s.push_str(&format!("  {:<38} {}\n", name, n));
        }
        s
    }
}

fn read_line(port: &mut Box<dyn SerialPort>, buf: &mut [u8]) -> std::io::Result<usize> {
    // 10 ms port timeout is a "no data yet" poll, not an error (same
    // convention as capture::Tap::read and protocol::Kline::read_chunk).
    match port.read(buf) {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
        Err(e) => Err(e),
    }
}

/// Build the dump image for a written span.
///
/// The 192 KB test write (0x10000..0x40000) is exported as a full 256 KB
/// image: the unwritten boot area 0x0..0x10000 is padded with 0xFF
/// (erased flash), so the dump lines up with a full flash read. Any other
/// span is dumped compactly, first to last written byte.
fn dump_image(flash: &[u8], lo: u32, hi: u32) -> Vec<u8> {
    if lo == 0x10000 && hi == 0x40000 {
        let mut img = flash[..0x40000].to_vec();
        img[..0x10000].fill(0xFF);
        img
    } else {
        flash[lo as usize..hi as usize].to_vec()
    }
}

fn tx(port: &mut Box<dyn SerialPort>, frame: &[u8], pacing: Duration) -> Result<()> {
    for b in frame {
        port.write_all(&[*b])
            .with_context(|| "serial write failed")?;
        port.flush().with_context(|| "serial flush failed")?;
        if !pacing.is_zero() {
            std::thread::sleep(pacing);
        }
    }
    Ok(())
}

pub fn run(opts: &RespondOptions) -> Result<Summary> {
    let mut port = serialport::new(&opts.port, opts.baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::Even)
        .stop_bits(StopBits::One)
        .timeout(Duration::from_millis(10))
        .open()
        .with_context(|| format!("cannot open {}", opts.port))?;
    port.clear(ClearBuffer::All)
        .with_context(|| "cannot flush stale line state")?;
    capture::install_sigint();
    STOP.store(false, Ordering::Relaxed);

    let mut file: Option<std::io::BufWriter<std::fs::File>> = match &opts.out {
        Some(p) => Some(
            std::io::BufWriter::new(
                std::fs::File::create(p)
                    .with_context(|| format!("cannot create log file {}", p.display()))?,
            ),
        ),
        None => None,
    };

    let t0 = Instant::now();
    let deadline = opts.duration.map(|d| t0 + d);

    let (mut sim, flash_desc) = match &opts.flash {
        Some(p) => {
            let sim = SimEcu::from_flash_file(p)
                .with_context(|| format!("flash image {}", p.display()))?;
            let n = crate::protocol::IMAGE_SIZE;
            (sim, format!("{} ({} bytes, served verbatim)", p.display(), n))
        }
        None => (SimEcu::new(), "built-in deterministic pattern".to_string()),
    };

    // 0x91 baud patch: configure the sim like the patched real ECU. The
    // patch's FIXED rate is the ECU's actual divisor (125000 → 124800),
    // the same mapping the tester side uses to reopen the port.
    let mut link_baud = opts.baud;
    let baud_patch_desc = if let Some(req) = opts.baud_patch {
        let actual = crate::protocol::reopen_divisor(req);
        sim.set_baud_patch(actual);
        format!("{req} (switches to {actual} on 0x91)")
    } else {
        String::new()
    };

    println!("dde3-toolkit respond — FAKE ECU (full duplex on one port)");
    println!(
        "port={} baud={} 8E1  reply-delay={} ms  tx-pacing={} ms  flash={}  (Ctrl-C to stop)",
        opts.port, opts.baud, opts.reply_delay_ms, opts.data_delay_ms, flash_desc
    );
    if !baud_patch_desc.is_empty() {
        println!("baud-patch: {baud_patch_desc} (0x91 accepted, link switches, ACK at new rate)");
    }
    if let Some(f) = file.as_mut() {
        let _ = writeln!(
            f,
            "# dde3-toolkit respond — FAKE ECU (full duplex on one port)"
        );
        let _ = writeln!(
            f,
            "# port={} baud={} 8E1  flash={}",
            opts.port, opts.baud, flash_desc
        );
        if !baud_patch_desc.is_empty() {
            let _ = writeln!(f, "# baud-patch: {baud_patch_desc}");
        }
        let _ = f.flush();
    }
    let mut framer = Framer::new();
    let mut last_tx: Option<Vec<u8>> = None;
    let mut frames: u64 = 0;
    let mut replies: u64 = 0;
    let mut raw_bytes: u64 = 0;
    let mut services: BTreeMap<String, u64> = BTreeMap::new();
    let mut buf = [0u8; 512];

    let mut emit = |line: &str| {
        println!("{line}");
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    };

    let dump_path = opts.dump.clone();

    while !STOP.load(Ordering::Relaxed) {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }

        let now = Instant::now();
        let n = match read_line(&mut port, &mut buf) {
            // SIGINT interrupts the blocking read; break to the summary/dump
            // path instead of surfacing `Interrupted system call` as fatal.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if STOP.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            other => other?,
        };
        let mut events = if n > 0 {
            framer.feed(&buf[..n], now)
        } else {
            Vec::new()
        };
        if let Some(ev) = framer.stale(now) {
            events.push(ev);
        }

        for ev in events {
            let t = now.duration_since(t0).as_secs_f32();
            match &ev {
                Event::Frame { payload } => {
                    // Bus echo of our own reply: the RX sees the TX, and the
                    // reply payload never collides with a peer request
                    // (requests are `12 <len> <sid> …` without a status
                    // byte at [2], replies carry the JOBRESULT at [2]).
                    if Some(payload.as_slice()) == last_tx.as_deref() {
                        emit(&format!(
                            "[{:9.3}] ECHO {}",
                            t,
                            crate::protocol::hex(&build_frame(payload))
                        ));
                        continue;
                    }
                    let name = capture::name_service(payload);
                    emit(&format!(
                        "[{:9.3}] RX   {}   {}",
                        t,
                        crate::protocol::hex(&build_frame(payload)),
                        name
                    ));
                    frames += 1;
                    *services.entry(name.to_string()).or_insert(0) += 1;
                    let reply = sim.handle(payload);
                    // 0x91 baud patch: the patched ECU switches the link
                    // BEFORE the ACK is transmitted (S0BG write, then ACK) —
                    // apply the switch, then the ACK rides the new rate.
                    if let Some(new_rate) = sim.take_baud_switch() {
                        port.set_baud_rate(new_rate)
                            .with_context(|| "cannot switch baud after 0x91")?;
                        port.clear(ClearBuffer::All)
                            .with_context(|| "cannot flush line after baud switch")?;
                        let old = link_baud;
                        link_baud = new_rate;
                        emit(&format!(
                            "[{t:9.3}] BAUD {old} -> {new_rate}   0x91 patch: link switched, ACK at new rate"
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(opts.reply_delay_ms));
                    let wire = build_frame(&reply);
                    tx(
                        &mut port,
                        &wire,
                        Duration::from_millis(opts.data_delay_ms),
                    )?;
                    last_tx = Some(reply);
                    replies += 1;
                    emit(&format!(
                        "[{:9.3}] TX   {}   REPLY",
                        Instant::now().duration_since(t0).as_secs_f32(),
                        crate::protocol::hex(&wire)
                    ));
                }
                Event::Raw(bytes) => {
                    let ascii: String = bytes
                        .iter()
                        .filter(|b| matches!(**b, 0x20..=0x7E))
                        .map(|b| *b as char)
                        .collect();
                    let ascii = if !ascii.is_empty() && ascii.len() == bytes.len() {
                        format!("  \"{}\"", ascii)
                    } else {
                        String::new()
                    };
                    emit(&format!(
                        "[{:9.3}] RAW  {}{}",
                        t,
                        crate::protocol::hex(bytes),
                        ascii
                    ));
                    raw_bytes += bytes.len() as u64;
                }
            }
        }
    }

    // Reconstruct the binary the peer wrote: dump the flash span it
    // programmed (the flasher's image, contiguous from the first to the
    // last written byte; the 192 KB test write 0x10000..0x40000 is
    // exported as a full 256 KB image — see `dump_image`).
    if let Some(path) = &dump_path {
        match sim.written_span() {
            Some((lo, hi)) => {
                let padded = lo == 0x10000 && hi == 0x40000;
                let img = dump_image(sim.flash_image(), lo, hi);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                match std::fs::write(path, &img) {
                    Ok(_) => {
                        let msg = if padded {
                            format!(
                                "reconstructed image: {} bytes (0x0000..0x40000, 0x0000..0x10000 padded FF) -> {}",
                                img.len(),
                                path.display()
                            )
                        } else {
                            format!(
                                "reconstructed image: {} bytes ({:#06x}..{:#06x}) -> {}",
                                img.len(), lo, hi, path.display()
                            )
                        };
                        println!("{msg}");
                        if let Some(f) = file.as_mut() {
                            let _ = writeln!(f, "# {msg}");
                            let _ = f.flush();
                        }
                    }
                    Err(e) => eprintln!("warning: cannot write {}: {e}", path.display()),
                }
            }
            None => {
                println!("reconstructed image: no flash writes received; nothing to dump");
            }
        }
    }

    if link_baud != opts.baud {
        println!("link left at {link_baud} (revert by restarting respond = power cycle)");
    }

    let summary = Summary {
        frames,
        replies,
        raw_bytes,
        services,
        elapsed: Instant::now().duration_since(t0),
    };
    let rendered = summary.render();
    print!("{rendered}");
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{rendered}");
        let _ = f.flush();
    }
    Ok(summary)
}

#[cfg(test)]
mod tests;
