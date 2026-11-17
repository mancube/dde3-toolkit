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
- **Checksum** — Bosch TDI V3.1 verify/correct (calibration + code slots)
  plus the cold-start **program chain** at `0x38B7C`, which any edit in the
  program area invalidates
- **Seed/key** — `12 08 90 42 4D 57 05 <Z>`, Z = seconds mod 46
- **Break-entered service protocol** — UART break → 10.4 k → 38.4 k with the
  decoded byte-addressed command set (`breakmode.rs`)
- **Firmware patches** — manifest-driven apply/revert of the shipped baud
  (`0x91`) and EWS-delete patches; fit is decided by the manifest's `before`
  bytes (any read of the same calibration works), checksums — including the
  program chain — are recomputed on apply, and an already-patched read with a
  stale checksum is repaired; builders and prebuilt images in
  [`patches/`](patches)
- **Protocol log** — every frame with timing; per-operation or continuous,
  to file and CSV
- **9-tab GUI** — Overview, Read, Write, Checksum, Patches, Fault codes,
  Live, Logger, Log (egui)
- **Headless bench** — `bench` subcommand for scripted runs
- **In-process ECU simulator** — `--sim` on any subcommand, or `--demo` in
  the GUI (no hardware needed)

## Build

```sh
cargo build --release
# → target/release/dde3-toolkit
```

## Releases

Prebuilt binaries for Linux and Windows live in [`dist/`](dist) (committed to
git, tagged `vX.Y.Z`), each with a `.sha256` file:

- `dist/dde3-toolkit-linux-x86_64`
- `dist/dde3-toolkit-windows-x86_64.exe`

To install a release, check out the tag and verify the binary:

```sh
git clone https://git.nas.lan/ikac/dde3-toolkit.git
cd dde3-toolkit && git checkout v0.1
sha256sum -c dist/dde3-toolkit-*.sha256   # run from the repo root
```

To cut a new release (bumps `Cargo.toml`, builds both targets, updates
`dist/`, commits, tags, pushes and creates the Gitea release with the
binaries as downloadable assets):

```sh
scripts/release.sh 0.2
```

## Windows build

Cross-compiled from Linux with `cargo-xwin` and a static CRT (same recipe as
dde4-toolkit):

```sh
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=/DELAYLOAD:KERNEL32.dll -C link-arg=/DEFAULTLIB:delayimp" \
  cargo xwin build --release --target x86_64-pc-windows-msvc
```

The `/DELAYLOAD:KERNEL32.dll` + delay-load helper keeps the release startable
on **Windows 7** (the eframe/winit GUI stack imports the Win8+-only
`GetSystemTimePreciseAsFileTime` from `KERNEL32.dll`, which Windows 7 does
not export — without the delay-load the binary died at startup with
`The procedure entry point GetSystemTimePreciseAsFileTime could not be located
in the dynamic link library KERNEL32.dll`).

On Windows use a COM port, e.g. `dde3-toolkit.exe --port COM3`.

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

`respond` is the fake-ECU mode: full duplex on ONE port (default
`/dev/ttyUSB0`) — it answers every received frame with the built-in
simulator (IDENT, STATUS, SEED/KEY, flash, DTC, live data), so a flashing
tool on the same K node talks to the sim while every frame and reply is
logged. Own replies are matched on the wire and logged as `ECHO`.

```sh
# Fake ECU on the tap port, mirroring the session to a log file
./target/release/dde3-toolkit respond --out fake_ecu.log

# Same, but serve a real flash dump: the tool's FLASH_LESEN requests are
# answered with the file's bytes (256 KB / 0x40000 image required)
./target/release/dde3-toolkit respond --out fake_ecu.log \
    --flash captures/actual_flash.bin
```

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

### GUI

The loaded image is shown in a status strip on the Write, Checksums and
Patches tabs (name, size/kind, checksum state) with a **Close image** control,
and in the side nav while it is loaded. Keyboard: **Ctrl+O** opens an image,
**Ctrl+S** saves the loaded image. Disabled controls explain why on hover.

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
| Fake-ECU responder (`respond`) | [`docs/respond-mode.md`](docs/respond-mode.md) |
| Fault/DTC protocol + record layout | [`docs/dtc-fault-protocol.md`](docs/dtc-fault-protocol.md) |
| Checksums (TDI V3.1 + program chain) | [`docs/checksum.md`](docs/checksum.md) |
| Firmware patches (baud, EWS delete) | [`docs/patches.md`](docs/patches.md), [`patches/README.md`](patches/README.md) |
| Break-entered service protocol | [`docs/break-mode.md`](docs/break-mode.md) |
| Reverse-engineering notes (VM, jobs) | [`docs/reverse-engineering.md`](docs/reverse-engineering.md) |
| Historical session log | [`docs/session-log.md`](docs/session-log.md) |

## Known limits

- No fault-**read** job exists in the stock tester program; fault reading
  uses the DS2 diagnostic program (`DDE30DS0.prg`) instead.
- The checksum-status reply was captured (`12 04 0A` → `12 05 A0 00`,
  2026-11-10), but the program-chain correction and the patched images have
  not been hardware-tested — bench-test before flashing a patched image.
- The 38.4 k break-mode secondary commands are decoded from the firmware but
  still pending one hardware capture.

## Safety

- ECU powered, engine stopped.
- Writes require a verified full backup and typing `WRITE`.
- Checksums **are** corrected on the write path (TDI code + calibration,
  then the cold-start program chain) with byte-for-byte parity against the
  verified reference tools — but the corrected/patched images themselves are
  bench-untested.
- Do not interrupt power during erase or programming.

## Relationship to dde4-toolkit

`dde4-toolkit` (Rust/egui) is the architectural and feature reference.
dde3-toolkit mirrors its structure but uses DDE3 DS2 service codes instead of
DDE4's UDS/KWP2000 codes, keeps the K-line at 9600 baud, and gets its DTC
tables from the embedded `.prg` instead of external files. DDE4's
`patches.rs` design is mirrored in `src/patches.rs`, with DDE3's own manifests
(baud `0x91`, EWS delete) and its three checksums.
