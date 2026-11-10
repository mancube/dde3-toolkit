# dde3-toolkit

Rust CLI + egui GUI tool for BMW EDC15 **DDE 3.0** (M47) K-line
diagnostics. Mirrors the `dde4-toolkit` feature set, adapted to the DDE3
protocol, which was reverse-engineered from `10DDE30.prg` and
`DDE30DS0.prg` (WinkFP bytecode) with a custom Python VM
(`tools/prg_vm.py`). See `HANDOVER.md` for the full narrative.

> **Naming: EDC15M and DDE3 are the same ECU.** Bosch EDC15M is the chip /
> family name; "DDE 3.0" is the hardware name the bench tools use. The
> reference Python checksum tool (`edc15m-checksum-tool/`) works on the exact
> reads this toolkit produces — "EDC15M" in the docs means this ECU.

## Features

- **Flash read** — full 256 KB or the 32 KB calibration region (0x38000–0x3FFFF)
- **Flash write** — full image (checksum-corrected) or calibration region,
  with UPROG_EIN → erase → block write → write-end sequence, progress
  reporting, and a post-write power-cycle modal
- **DTCs (fault memory)** — read, freeze values, shadow values, text
  (ORT/FArt tables embedded in `DDE30DS0.prg`), DS2 and K-line clear
- **Live data** — DDE3 MW service (measurement groups via ADF ids)
- **Seed/key** — `12 08 90 42 4D 57 05 <Z>`, Z = seconds mod 46
- **Checksum** — TDI V3.1 verify/correct (cal + code blocks)
- **Protocol log** — every frame with timing, per-operation and continuous
  logging (file + CSV)
- **Headless bench** — `bench` subcommand for scripted reads/writes
- **8-tab GUI** — Overview, Read, Write, Checksum, Fault codes, Live,
  Logger, Log

## Protocol constants (from VM extraction + wire captures)

| Constant | Value | Source |
|---|---|---|
| ECU address | `0x12` | xsend templates start with 0x12 |
| Wire frame | `<DS2 payload> <XOR>` | CONFIRMED, passive capture (HANDOVER §17) |
| Default baud | 9600 8E1 | xsetpar `80 25`; DDE3 K-line stays at 9600 |
| Flash base | `0x00000` | DDE3 reads from address 0 |
| Flash size | 256 KB (0x40000) | HANDOVER §14 |
| Calibration | 0x38000, 32 KB | checksum.rs |
| ECU reply | `92 <len> <SB> <data…> <XOR>`, SB 0xA0/0xA1/0xFF | capture 2026-11-09 |
| ECU identity | "DDE 3.0 fuer M47" | capture 2026-11-09 |

**Wire framing:** no `0xB8` prefix, no tester address, no length byte — the
frame is the DS2 payload plus a trailing XOR byte, delimited by line
silence. ECU replies: `92 <len-echo> <SB> <data…>` + trailing XOR, with
SB = 0xA0 (OK), 0xA1 (busy, retried), 0xFF (NACK).

## Known frames (DS2 payloads)

| Job | Frame | Notes |
|---|---|---|
| UPROG_EIN | `12 04 0D` | Enter programming mode (required before erase/write) |
| IDENT | `12 04 00` | ECU identification → "DDE 3.0 fuer M47" |
| Checksum status | `12 04 0A` | In-band checksum status |
| DIAGNOSE_ENDE | `12 04 9F` | Leave session |
| SEED_KEY | `12 08 90 42 4D 57 05 <Z>` | Z = seconds mod 46 |
| Fault count | `12 05 14 01` | → `<n>` (DS2 diagnostic program) |
| Fault entry | `12 05 04 00` | → `<ort> <art> <uw1:2> <uw2:2>`, iterates |
| Fault shadow | `12 05 04 01` | → 11 × u16 BE freeze values |
| Fault clear (DS2) | `12 04 05` | Clear fault memory |
| Fault clear (K-line) | `12 09 07 0E 42 41 50 00` | "BAP" clear, independent path |
| DS2→K2 protocol switch | `12 05 90 56` | seen live 2026-11-09 |
| FLASH_LESEN | `12 09 06 <A> <count>` | Read flash at address |
| FLASH_LOESCHEN | `12 09 07 06 <A> <count>` | Erase flash |
| FLASH_SCHREIBEN | `12 09 07 02 <A> <data…>` | 128-byte block write |
| FLASH_SCHREIBEN_ENDE | `12 09 07 0F <A> <00>` | End write |
| MW live data | `12 05 0B 10 <id_hi> <id_lo>` | ADF id (e.g. 0x0F65 battery) |

## DTC tables

Embedded in `DDE30DS0.prg` (body XOR 0xF7): ORT (48 fault classes),
FArtMatrix (47 × 16), FArtTexte (97 fault texts), FUmweltTexte (11 freeze
params with `value = A·raw + B` scaling). `src/dtc.rs` is **generated** —
regenerate with:

```sh
python3 tools/gen_dtc_rs.py --prg /path/to/DDE30DS0.prg
```

## Layout

```
dde3-toolkit/
├── Cargo.toml
├── README.md
├── HANDOVER.md          # reverse-engineering narrative (§18 = latest status)
├── SESSION_2026-11-09.md
├── src/
│   ├── main.rs          # CLI entry + GUI launcher
│   ├── protocol.rs      # DDE3 frame codec, Kline, all services
│   ├── checksum.rs      # TDI V3.1 flash checksum
│   ├── dtc.rs           # GENERATED DTC tables + text/scaling helpers
│   ├── sim.rs           # in-process ECU simulator (flash + fault memory)
│   ├── bench.rs         # headless bench runner
│   └── app.rs           # egui GUI (8 tabs)
├── tools/               # Python VM + extraction tools
│   ├── prg_vm.py        # WinkFP bytecode VM
│   ├── extract_frames.py
│   ├── extract_param_frames.py
│   ├── extract_ds_frames.py
│   ├── disasm.py        # full EDIC15/EDIABAS disassembler
│   ├── gen_dtc_rs.py    # regenerates src/dtc.rs from DDE30DS0.prg
│   └── monitor.py       # passive wire monitor (UART-LPT adapter)
└── analysis/
    ├── frames.md        # extracted wire frames per job
    ├── ds_frames.md     # DS2 fault-service frames
    ├── prg_ocnames.json / prg_sizes.json
    ├── winkfp_proto.txt
    └── captures/        # raw wire captures (2026-11-09)
```

## Build

```sh
cargo build --release
```

Produces `target/release/dde3-toolkit`.

## Run

No arguments launches the egui GUI:

```sh
./target/release/dde3-toolkit
```

CLI subcommands (all take `--port <path>`, `--baud <rate>`,
`--data-delay <ms>`, and `--sim` to use the in-process ECU):

```sh
# Identify the ECU
dde3-toolkit ident --port /dev/ttyUSB1

# Read full 256 KB flash
dde3-toolkit read flash.bin --port /dev/ttyUSB1

# Read calibration area (0x38000–0x3FFFF)
dde3-toolkit read-cal cal.bin --port /dev/ttyUSB1

# Fault codes (count, entries, text, freeze values)
dde3-toolkit dtc --port /dev/ttyUSB1

# Clear faults (K-line "BAP" clear / DS2 clear)
dde3-toolkit fault-clear --port /dev/ttyUSB1
dde3-toolkit ds2-clear --port /dev/ttyUSB1

# Seed/key
dde3-toolkit seed-key 0x05 --port /dev/ttyUSB1

# Verify/correct checksums
dde3-toolkit checksum flash.bin --port /dev/ttyUSB1

# Full write (requires typing WRITE to confirm)
dde3-toolkit write flash.bin --port /dev/ttyUSB1
dde3-toolkit write-cal cal.bin --port /dev/ttyUSB1

# Headless bench
dde3-toolkit bench --sim --read --out /tmp/full.bin
dde3-toolkit bench --sim --read-cal --out /tmp/cal.bin
dde3-toolkit bench --sim --write-cal --image /tmp/cal.bin
```

Bench timing (sim, 9600 8E1 + 3 ms/byte pacing): read 256 KB ≈ 98 s,
read-cal ≈ 12 s, write-cal ≈ 4 min, full write ≈ 8.5 min —
protocol-inherent, not a bottleneck in the tool.

### GUI automation

The GUI accepts the same automation flags as dde4-toolkit (any one of
them switches a flag-style invocation into GUI mode):

```sh
./target/release/dde3-toolkit --autoconnect --port /dev/ttyUSB1 --auto-read
./target/release/dde3-toolkit --port /dev/ttyUSB1 --auto-write --image flash.bin
./target/release/dde3-toolkit --autoconnect --auto-log /tmp/log.csv
./target/release/dde3-toolkit --autoconnect --auto-live
./target/release/dde3-toolkit --tab dtc --show-vars --width 1400 --height 900
./target/release/dde3-toolkit --demo          # fake ECU, no hardware
```

Flags: `--autoconnect`, `--auto-read`, `--auto-cal`, `--auto-live`,
`--auto-log <path>`, `--auto-write-cal`, `--auto-write`, `--fast-write`,
`--show-vars`, `--tab <name>`, `--width <f>`, `--height <f>`, `--demo`.

After connect, exactly one auto-action runs: write > write-cal > read >
cal > log > live (first match wins). Battery voltage is snapshotted on
connect and before every read/write, shown as a pill on the Read/Write
tabs and on the Fault codes tab.

**Bench wiring (dual-adapter):** `/dev/ttyUSB1` (FT232R) drives the K-line,
`/dev/ttyUSB0` (QinHeng UART-LPT) taps it passively — run
`python3 tools/monitor.py` on it to watch real wire traffic.

## Checksum status

TDI V3.1 (Bosch EDC15M) algorithm ported from the verified Python
reference; self-tested against regression vectors
(calibration `0x007BC5EB`, code `0x2477887F`). Block layout: calibration
`0x38000–0x3FFFF` (checksum at `0x7BFC`), code area `0x10000–0x14000`
(checksum at `0x13FFC`). ⚠️ A real-ECU checksum-status reply has not been
captured yet — confirm on hardware before flashing corrected images.

## Testing

```sh
cargo test
```

25 tests: frame builders vs VM-extracted frames, parser resync/timeout
behaviour, checksum determinism and idempotent correction, DTC table
shape/text/scaling, bench image preparation.

## Safety

- ECU powered, engine stopped.
- Write requires a verified full backup and typing `WRITE`.
- Checksums are NOT auto-corrected for DDE3 writes (algorithm
  hardware-unverified).
- Do not interrupt power during erase or programming.

## Relationship to dde4-toolkit

Reference: `EDC15C4-decomp/dde4-toolkit/` (Rust/egui, ~4900 lines).
dde3-toolkit mirrors its structure and feature set but uses DDE3 DS2
service codes instead of DDE4's UDS/KWP2000 codes, keeps the K-line at
9600 baud, and gets its DTC tables from the embedded .prg instead of
external CSV/SGBD files. `patches.rs` (DDE4 binary-patch engine) is
intentionally **not** ported — no DDE3 patches exist.

## Status (2026-11-11)

✅ Wire framing, ECU identity, fault read/clear protocol, DTC tables,
checksum algorithm, GUI (8 tabs), bench runner, in-process ECU sim,
git repo (`main`, 3 commits).

⬜ Real-ECU verification of: DTC ORT/FArt/UMwelt mapping, checksum
status reply, MW live-data ADF ids (battery 0x0F65 confirmed).
