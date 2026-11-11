# dde3-toolkit

A CLI + GUI tool for talking to **BMW EDC15 "DDE 3.0"** engine ECUs over the
K-line (ISO 9141/14230, DS2) at 9600 baud. Written in Rust.

It reads and writes the ECU's flash memory, reads/clears fault codes, shows
live data, and logs the raw protocol — with an in-process ECU simulator
(`--sim`) so the full workflow can be exercised without hardware.

The DDE3 protocol has no public documentation; it was reverse-engineered
from the OEM WinKFP program `10DDE30.prg` and confirmed against real-car
captures. All of that work is written up in [`docs/`](docs) — start with
[`HANDOVER.md`](HANDOVER.md).

> **Naming note:** "EDC15M" and "DDE3" are the **same ECU**. Bosch EDC15M
> is the chip/family name; DDE 3.0 is the hardware name the bench tools use
> (here: an M47 engine). "EDC15M" in the docs always means this ECU.

## Features

- **Flash read** — full 256 KB or the 32 KB calibration region (0x38000–0x3FFFF)
- **Flash write** — full image or calibration region; UPROG_EIN → erase →
  block write → write-end, with progress reporting and a post-write
  power-cycle step
- **Fault codes (DTCs)** — read (4-step chunked protocol), freeze values,
  shadow values, fault texts (ORT/FArt tables embedded in the OEM
  `DDE30DS0.prg`), DS2 and K-line clear
- **Live data** — DDE3 MW service, batched into as few telegrams as the
  0x80-byte DS2 cap allows; variable list generated from the ECU's ADF table
- **Checksum** — Bosch TDI V3.1 verify/correct (calibration + code blocks)
- **Seed/key** — `12 08 90 42 4D 57 05 <Z>`, Z = seconds mod 46
- **Break-entered service protocol** — UART break → 10.4 k → 38.4 k with the
  decoded byte-addressed command set (`breakmode.rs`)
- **Protocol log** — every frame with timing; per-operation or continuous,
  to file and CSV
- **8-tab GUI** — Overview, Read, Write, Checksum, Fault codes, Live,
  Logger, Log (egui)
- **Headless bench** — `bench` subcommand for scripted runs
- **In-process ECU simulator** — `--sim` on any subcommand, or `--demo` in
  the GUI (no hardware needed)

## Build

```sh
cargo build --release
# → target/release/dde3-toolkit
```

## Quick start (no hardware)

```sh
# Run the full workflow against the built-in simulator
./target/release/dde3-toolkit bench --sim --read --out /tmp/full.bin
./target/release/dde3-toolkit --demo        # GUI with a fake ECU
```

## Using it with a real ECU

Bench wiring (dual adapter): `/dev/ttyUSB1` (FT232R) drives the K-line,
`/dev/ttyUSB0` (QinHeng UART-LPT) taps it passively —
`python3 tools/monitor.py` shows the real wire traffic on it.

```sh
# Identify the ECU (→ "DDE 3.0 fuer M47")
./target/release/dde3-toolkit ident --port /dev/ttyUSB1

# Read flash
./target/release/dde3-toolkit read flash.bin --port /dev/ttyUSB1
./target/release/dde3-toolkit read-cal cal.bin --port /dev/ttyUSB1

# Fault codes + clear
./target/release/dde3-toolkit dtc --port /dev/ttyUSB1
./target/release/dde3-toolkit fault-clear --port /dev/ttyUSB1

# Verify/correct TDI V3.1 checksums
./target/release/dde3-toolkit checksum flash.bin

# Write (requires typing WRITE to confirm)
./target/release/dde3-toolkit write flash.bin --port /dev/ttyUSB1
./target/release/dde3-toolkit write-cal cal.bin --port /dev/ttyUSB1
```

All subcommands also accept `--baud <rate>`, `--data-delay <ms>` and `--sim`.
With no arguments the binary launches the GUI; it accepts the same
`--autoconnect/--auto-read/--auto-write/--demo/...` automation flags as
dde4-toolkit. Full command list: see `HANDOVER.md` → docs, or just run the
binary with no subcommand.

## Testing

```sh
cargo test        # protocol frames, parser, checksum, DTC tables, break mode
```

## Repository layout

```
├── src/
│   ├── main.rs          # CLI entry + GUI launcher
│   ├── protocol.rs      # DDE3 frame codec, K-line I/O, all services
│   ├── checksum.rs      # TDI V3.1 flash checksum
│   ├── dtc.rs           # GENERATED DTC tables (tools/gen_dtc_rs.py)
│   ├── live.rs          # GENERATED live-data ADF table (tools/gen_live_rs.py)
│   ├── breakmode.rs     # break-entered 10.4k/38.4k service protocol
│   ├── sim.rs           # in-process ECU simulator
│   ├── bench.rs         # headless bench runner
│   └── app.rs           # egui GUI (8 tabs)
├── tools/               # Python: WinkFP VM, frame extraction, wire monitor
├── analysis/            # extracted frames + reference data from the .prg
├── docs/                # protocol findings, RE notes, session log
├── HANDOVER.md          # current status + the task at hand
└── README.md
```

## Documentation

| What | Where |
|---|---|
| Status + task at hand | [`HANDOVER.md`](HANDOVER.md) |
| Wire protocol (framing, replies, timing) | [`docs/wire-protocol.md`](docs/wire-protocol.md) |
| Fault/DTC protocol + record layout | [`docs/dtc-fault-protocol.md`](docs/dtc-fault-protocol.md) |
| Checksums (TDI V3.1) | [`docs/checksum.md`](docs/checksum.md) |
| Break-entered service protocol | [`docs/break-mode.md`](docs/break-mode.md) |
| Reverse-engineering notes (VM, jobs) | [`docs/reverse-engineering.md`](docs/reverse-engineering.md) |
| Historical session log | [`docs/session-log.md`](docs/session-log.md) |

## Known limits

- No fault-**read** job exists in the stock tester program; fault reading
  uses the DS2 diagnostic program (`DDE30DS0.prg`) instead.
- The real-ECU checksum-status reply has not been captured yet — confirm on
  hardware before flashing checksum-corrected images.
- The 38.4 k break-mode secondary commands are decoded from the firmware but
  still pending one hardware capture.

## Safety

- ECU powered, engine stopped.
- Writes require a verified full backup and typing `WRITE`.
- Checksums are **not** auto-corrected on the write path (algorithm
  hardware-unverified).
- Do not interrupt power during erase or programming.

## Relationship to dde4-toolkit

`dde4-toolkit` (Rust/egui) is the architectural and feature reference.
dde3-toolkit mirrors its structure but uses DDE3 DS2 service codes instead of
DDE4's UDS/KWP2000 codes, keeps the K-line at 9600 baud, and gets its DTC
tables from the embedded `.prg` instead of external files. DDE4's
`patches.rs` is deliberately not ported — no DDE3 patches exist.
