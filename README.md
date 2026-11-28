# dde3-toolkit

A CLI + GUI tool for talking to **BMW EDC15 "DDE 3.0"** engine ECUs over the
K-line (ISO 9141/14230, DS2) at 9600 baud. Written in Rust.

It reads and writes the ECU's flash memory, reads/clears fault codes, shows
live data, logs selected values to CSV, verifies TDI checksums, applies
firmware patches, and logs the raw protocol — with a fake-ECU responder
(`respond`) so the full workflow can be exercised without hardware.

The DDE3 protocol has no public documentation; it was reverse-engineered
from the OEM WinKFP program `10DDE30.prg` and confirmed against real-car
captures. All of that work is written up in [`docs/`](docs) — start with
[`HANDOVER.md`](HANDOVER.md).

> **Naming note:** "EDC15M" and "DDE3" are the **same ECU**. Bosch EDC15M
> is the chip/family name; DDE 3.0 is the hardware name the bench tools use
> (here: an M47 engine).

## Features

- **Flash read** — full 256 KB (read from 0x8000: 1792 × 128 B; the IROM
  prefix `[0, 0x8000)` always reads FF and is FF-padded) or the 32 KB
  calibration region (0x38000–0x3FFFF). Each run opens the full flasher
  session first (IDENT → UPROG_EIN → AIF read → SEED_KEY) before the
  `FLASH_LESEN` chunks, matching the Galletto read flow.
- **Flash write** — both flows verify and correct the flash checksums first
  and require typing `WRITE`:
  - *full image*: Galletto's two-session flow — program area
    (0x10000–0x38000) → `DIAGNOSE_ENDE` → re-authenticate → calibration
    area (0x38000–0x40000), 224-byte blocks in both regions (731×224 +
    1×96 program, 146×224 + 1×64 cal = 879 frames, down from 3072×64 B),
    per-area erase. The program-area 224 B size is a new experiment —
    on-car verification still pending (see Known limits).
  - *calibration only*: MPPS-style — DTC read+clear prep, a single `07 0F`
    write-end, then 224-byte blocks (146 × 224 + 1 × 64), data frames
    unpaced (≈330 ms/frame, MPPS parity). On-car-proven.
- **Fault codes (DTCs)** — 4-step chunked read, freeze values, shadow
  values, fault texts (ORT/FArt tables embedded in the OEM `DDE30DS0.prg`),
  DS2 clear (`ds2-clear`) and the legacy K-line BAP clear (`fault-clear`)
- **Live data** — DDE3 MW service (`0B 10`), batched into as few telegrams
  as the DS2 frame cap allows; 81 variables generated from the ECU's ADF
  table (`tools/gen_live_rs.py`); signed (i16) and unsigned (u16) ADFs
  decoded per the ADF data type; four view modes (line graph, plain labels,
  in-car arc gauges, value cards with sparklines) and a measured Hz counter
- **Value logger** — logs any selected set of live variables to CSV with
  physical values (unit-scaled, sign-correct), started/stopped from the GUI
  Logger tab; keeps its own variable selection, separate from the Live tab's
- **Variable presets** — named variable selections shared by the Live and
  Logger tabs (create / rename / delete in the variable picker); the last
  session's selections, presets and Live view mode are restored on launch
  (`~/.config/dde3-toolkit/presets.json`, `$DDE3_CONFIG` override)
- **Checksum** — Bosch TDI V3.1 verify/correct (code + calibration slots)
  plus the cold-start **program chain** at `0x38B7C`, which any edit in the
  program area invalidates; byte-for-byte parity with the verified reference
  tools
- **Seed/key** — `12 08 90 42 4D 57 05 <Z>`; the key is the job's unbiased
  formula on the 46-byte seed-reply wire frame (matches all captured
  Galletto vectors)
- **Fast baud (0x91)** — `baud --rate 38400|125000` switches the DS2 link
  up (needs the baud patch written first); live logging and flash
  read/write then run at the fast rate with pacing disabled
- **Firmware patches** — manifest-driven apply/revert of the shipped baud
  (`0x91`) and EWS-delete patches; fit is decided by the manifest's `before`
  bytes, all three checksums are recomputed on apply, and an already-patched
  read with a stale checksum is repaired; builders and prebuilt images in
  [`patches/`](patches)
- **Protocol log** — every frame with timing, to file and in the GUI (the
  in-app panel keeps the last 2000 lines). The GUI also writes a full,
  untruncated run log per launch to `<log dir>/dde3-<startup>.log` (path
  shown in the Log tab and on stderr). The log dir defaults to
  `~/.local/share/dde3-toolkit/logs/` (persistent, survives reboots); set
  `$DDE3_LOG_DIR` or pass `--log-dir <path>` to move it
- **Log analysis** — `log` analyzes the toolkit's own logs headlessly (Rust
  ports of the former Python tools): `log sum <capture.txt>… [--image
  <bin>]` summarizes the request sequences of respond/capture logs and
  verifies FLASH_SCHREIBEN runs (size histogram, address contiguity/coverage,
  per-frame ACK, timing; `--image` also checks the written data against a
  reference image); `log run <log>` pairs TX/RX, validates the envelope,
  summarizes per-command reply shapes/gaps and the FLASH_LESEN sequence
  (optionally verified against `--image <bin>`); `log write <log>` breaks a
  real-car write run into its note/error timeline, per-class TX pacing,
  TX→reply latency, the flash-write phase, and silence gaps — with an
  optional `--mpps <file>` reference capture for pacing comparison
- **9-tab GUI** — Overview, Read, Write, Checksum, Patches, Fault codes,
  Live, Logger, Log (egui)
- **Passive capture** — `capture` taps the K-line read-only (frames named,
  raw bytes kept verbatim; nothing is ever transmitted)
- **Fake-ECU responder** — `respond` runs the full `SimEcu` state machine
  on a serial port (IDENT, STATUS, SEED/KEY, flash, DTC, live data, 0x91
  baud), can serve a real flash dump (`--flash`) and reconstruct what a
  peer programmed (`--dump`); plus `--demo` in the GUI (no hardware needed)

## Build

```sh
cargo build --release
# → target/release/dde3-toolkit
```

## Releases

Prebuilt binaries for Linux and Windows 10/11 live in [`dist/`](dist)
(committed to git, tagged `vX.Y.Z`), each with a `.sha256` file:

- `dist/dde3-toolkit-linux-x86_64`
- `dist/dde3-toolkit-windows-x86_64.exe`

To install a release, check out the tag and verify the binary:

```sh
git clone https://git.nas.lan/ikac/dde3-toolkit.git
cd dde3-toolkit && git checkout v0.5.3
sha256sum -c dist/dde3-toolkit-*.sha256   # run from the repo root
```

To cut a new release (bumps `Cargo.toml`, builds the Linux and Windows
targets into `dist/`, commits, tags, pushes and creates the Gitea release
with the binaries as downloadable assets):

```sh
scripts/release.sh 0.5.4
```

## Quick start (no hardware)

```sh
# Fake ECU on a pty pair, then run the toolkit against it
socat -d -d pty,raw,echo=0,link=/tmp/ecu-pty pty,raw,echo=0,link=/tmp/tool-pty &
./target/release/dde3-toolkit respond --port /tmp/ecu-pty --out fake_ecu.log &
./target/release/dde3-toolkit read /tmp/full.bin --port /tmp/tool-pty
./target/release/dde3-toolkit --demo        # GUI with a fake ECU
```

See the `respond` section below for the full fake-ECU options.

## Using it with a real ECU

Bench wiring (dual adapter): `/dev/ttyUSB1` (FT232R) drives the K-line,
`/dev/ttyUSB0` (QinHeng UART-LPT) taps it passively —
`python3 tools/monitor.py` (or the `capture` subcommand) shows the real
wire traffic on it.

```sh
# Identify the ECU (→ "DDE 3.0 fuer M47")
./target/release/dde3-toolkit ident --port /dev/ttyUSB1

# Read flash
./target/release/dde3-toolkit read flash.bin --port /dev/ttyUSB1
./target/release/dde3-toolkit read-cal cal.bin --port /dev/ttyUSB1

# Fault codes + clear
./target/release/dde3-toolkit dtc --port /dev/ttyUSB1
./target/release/dde3-toolkit ds2-clear --port /dev/ttyUSB1

# Verify/correct TDI V3.1 checksums
./target/release/dde3-toolkit checksum flash.bin

# Write (requires typing WRITE to confirm)
./target/release/dde3-toolkit write flash.bin --port /dev/ttyUSB1
./target/release/dde3-toolkit write-cal cal.bin --port /dev/ttyUSB1

# Switch the link to fast baud (needs the baud patch written first)
./target/release/dde3-toolkit baud --rate 125000 --port /dev/ttyUSB1
```

The K-line subcommands also accept `--port`, `--baud <rate>` and
`--data-delay <ms>` (per-byte pacing of control frames; flash data frames
are unpaced). More single-purpose commands: `seed-key <Z>`, `uprog-ein`,
`diagnose-ende`, `fault-clear`, `capture`, `checksum`, `patches`,
`log <sum|run|write>`.
With no arguments (or a leading flag) the binary launches the GUI.

### `respond` — the fake ECU

Full duplex on ONE port (default `/dev/ttyUSB0`): it answers every received
frame like an EDC15 would, so a flashing tool (or this toolkit) on the
same K node talks to the sim while every frame and reply is logged. Own
replies are matched on the wire and logged as `ECHO`.

```sh
# Fake ECU on the tap port, mirroring the session to a log file
./target/release/dde3-toolkit respond --out fake_ecu.log

# Same, but serve a real flash dump: FLASH_LESEN + the AIF read return
# the file's bytes (256 KB image required)
./target/release/dde3-toolkit respond --out fake_ecu.log \
    --flash captures/actual_flash.bin

# Reconstruct what the peer programmed (192 KB test writes are
# FF-padded to a full 256 KB image)
./target/release/dde3-toolkit respond --out fake_ecu.log --dump written.bin

# Behave like the patched real ECU (0x91 baud change): accept the 0x91,
# switch the port to the patch's rate (125000 -> 124800) and ACK at the
# new rate, so `baud --rate …` and the fast-baud read/write run end-to-end
./target/release/dde3-toolkit respond --out fake_ecu.log \
    --patched-baud 125000
```

### GUI

Runs with no arguments (or a leading flag): `dde3-toolkit [--demo]
[--autoconnect] [--auto-live] [--auto-read] [--auto-cal] [--auto-log
<csv>] [--auto-write-cal] [--auto-write] [--image <bin>] [--fast-baud
<rate>] [--port <path>] [--tab <name>] [--log-dir <path>] [--show-vars]
[--width <px>] [--height <px>]` (`--show-vars` opens with the live-variable
picker up).

- The loaded image is shown in a status strip on the Write, Checksums and
  Patches tabs (name, size/kind, checksum state) with a **Close image**
  control, and in the side nav while it is loaded. Keyboard: **Ctrl+O**
  opens an image, **Ctrl+S** saves the loaded image. Disabled controls
  explain why on hover.
- The side nav has a program-wide **LINK · GLOBAL** section: the
  "faster 9600" live-polling switch and the fast-baud (0x91) button. The
  battery voltage pill sits in the top bar next to the CONNECTED status.
- The variable picker windows (Live and Logger tabs) have a preset row:
  pick a saved preset and **Apply** it to that tab's selection, or
  **Save as…** / **Rename…** / **Delete** presets.

## Testing

```sh
cargo test        # unit + E2E fake-ECU tests
```

- **Unit tests**: protocol frames, parser, checksum, DTC, live decode, sim.
- **E2E fake-ECU tests** (`tests/e2e_fake_ecu.rs`): full read, calibration
  read, full write and calibration write — the real CLI binary driven
  against the `respond` fake ECU over a socat pty bridge, with the
  reconstructed flash image byte-compared against the source. They are
  **not definitive** (fake ECU — only on-car runs prove what the real ECU
  accepts) and they need a real 256 KB flash dump at
  `captures/actual_flash.bin` (private — never committed; put your own
  there) and `socat` installed. Without either, the tests skip and say so.

## Repository layout

```
├── src/
│   ├── main.rs          # CLI entry + GUI launcher
│   ├── protocol.rs      # DDE3 frame codec, K-line I/O, all services
│   ├── checksum.rs      # TDI V3.1 flash checksum + program chain
│   ├── dtc.rs           # GENERATED DTC tables (tools/gen_dtc_rs.py)
│   ├── live.rs          # GENERATED live-data ADF table (tools/gen_live_rs.py)
│   ├── sim.rs           # ECU simulator (the `respond` fake-ECU state machine)
│   ├── respond.rs       # the `respond` fake-ECU server (--flash/--dump/--patched-baud)
│   ├── capture.rs       # passive K-line capture (read-only tap)
│   ├── patches.rs       # manifest-driven firmware patches
│   ├── logana.rs        # the `log` subcommand (sum/run/write log analyzers)
│   ├── presets.rs       # variable presets + persisted GUI state
│   └── app.rs           # egui GUI (9 tabs)
│   (each module's tests live in src/<name>/tests.rs)
├── patches/             # patch builders + manifests + prebuilt images
├── tools/               # Python: WinkFP VM, frame extraction, wire monitor,
│                        #   table generators
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
| K-line RX buffer & frame limits | [`docs/ds2-rx-buffer.md`](docs/ds2-rx-buffer.md) |
| Fake-ECU responder (`respond`) | [`docs/respond-mode.md`](docs/respond-mode.md), [`docs/simulator.md`](docs/simulator.md) |
| Fault/DTC protocol + record layout | [`docs/dtc-fault-protocol.md`](docs/dtc-fault-protocol.md) |
| Checksums (TDI V3.1 + program chain) | [`docs/checksum.md`](docs/checksum.md) |
| Fast baud (0x91) patch | [`docs/baud-0x91-patch.md`](docs/baud-0x91-patch.md) |
| MPPS vs Galletto calibration flows | [`docs/FINDINGS_mpps_vs_galletto_calibration.md`](docs/FINDINGS_mpps_vs_galletto_calibration.md) |
| Firmware patches (baud, EWS delete) | [`docs/patches.md`](docs/patches.md), [`patches/README.md`](patches/README.md) |
| Reverse-engineering notes (VM, jobs) | [`docs/reverse-engineering.md`](docs/reverse-engineering.md) |
| Real-car capture analysis (2026-11-19) | [`docs/archive/SESSION_2026-11-19.md`](docs/archive/SESSION_2026-11-19.md) (archived) |
| Historical session log | [`docs/session-log.md`](docs/session-log.md) |

## Known limits

- No fault-**read** job exists in the stock tester program; fault reading
  uses the DS2 diagnostic program (`DDE30DS0.prg`) instead. The rewritten
  4-step fault read still awaits one real-car confirmation.
- The fast-baud (0x91) path is bench-verified against the fake ECU
  end-to-end; on-car use requires the baud patch and is not yet
  car-verified.
- 224-byte (calibration write) / 128-byte (read) are the on-car-verified
  block sizes. The full write now also uses 224-byte blocks in the program
  area (wire-protocol §37) — a deliberate new experiment, distinct from the
  rejected 192-B mode that hung the ECU (§32/§33). If the ECU hangs at
  0x10000, `write_full` reverts to the 64-byte `WRITE_BLOCK` reference
  size.

## Safety

- ECU powered, engine stopped.
- Writes require a verified full backup and typing `WRITE`.
- Checksums **are** corrected on the write path (TDI code + calibration,
  then the cold-start program chain) with byte-for-byte parity against the
  verified reference tools — but bench-test a corrected/patched image
  before flashing a car ECU.
- Do not interrupt power during erase or programming.

## Relationship to dde4-toolkit

`dde4-toolkit` (Rust/egui) is the architectural and feature reference.
dde3-toolkit mirrors its structure but uses DDE3 DS2 service codes instead of
DDE4's UDS/KWP2000 codes, keeps the K-line at 9600 baud (with the optional
0x91 fast-baud patch), and gets its DTC tables from the embedded `.prg`
instead of external files. DDE4's `patches.rs` design is mirrored in
`src/patches.rs`, with DDE3's own manifests (baud `0x91`, EWS delete) and
its three checksums.
