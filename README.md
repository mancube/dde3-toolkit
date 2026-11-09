# dde3-toolkit

Rust CLI tool for BMW EDC15 DDE3.0 K-line diagnostics. Replicates the
`dde4-toolkit` feature set for the DDE3 protocol: full/partial flash read,
flash write, seed/key, fault clear, checksum verify/correct, and protocol
logging.

The DDE3 protocol is reverse-engineered from `10DDE30.prg` (a WinkFP
bytecode program) using a custom Python VM (`dde3-toolkit/tools/prg_vm.py`).
See `HANDOVER.md` for the full reverse-engineering narrative.

```
dde3-toolkit/
├── Cargo.toml
├── README.md
├── HANDOVER.md          # reverse-engineering narrative + open questions
├── src/
│   ├── main.rs          # CLI entry + GUI launcher
│   ├── protocol.rs      # DDE3 frame codec, K-line connection, all known frames
│   ├── checksum.rs      # TDI V3.1 flash checksum (UNVERIFIED for DDE3)
│   └── app.rs           # egui GUI (Read, Write, Checksums, Live, Logger, Log)
├── tools/               # Python VM + frame extraction (see ../HANDOVER.md)
│   ├── prg_vm.py
│   ├── extract_frames.py
│   ├── extract_param_frames.py
│   └── test_uprog_ein.py
└── analysis/
    └── frames.md        # extracted wire frames per job
```

## Protocol constants (from VM extraction)

| Constant | Value | Source |
|---|---|---|
| ECU address | `0x12` | xsend templates start with 0x12 |
| Wire frame | `<DS2 payload> <XOR>` | CONFIRMED — passive capture 2026-11-09 (see below) |
| Default baud | 9600 8E1 | from xsetpar `80 25` = 9600 |
| Flash base | `0x00000` | DDE3 reads from address 0 |
| Flash size | 256 KB (0x40000) | HANDOVER.md §14 |

**✅ Wire framing confirmed (2026-11-09, HANDOVER.md §17).** A passive sniffer
on the K-line (WinKFP driving the bus) shows the on-wire frame is simply the
DS2 payload plus a trailing XOR byte — **no `0xB8` prefix, no tester address,
no length byte** (the earlier `B8 12 F1 LEN …` hypothesis was wrong). Frames
are delimited by line silence; WinKFP gaps ~50 ms before the trailing XOR
byte. Captured examples: `12 04 0D 1B` (UPROG_EIN), `12 04 00 16` (IDENT),
`12 05 90 56 D1` (SCHALTE_PROTOKOLL_DS2_NACH_K2).

## Known frames (DS2 payloads)

All extracted from `tools/analysis/frames.md` via the WinkFP VM:

| Job | Frame | Notes |
|---|---|---|
| UPROG_EIN | `12 04 0D` | Enter programming mode |
| IDENT | `12 04 00` | ECU identification |
| Checksum status | `12 04 0A` | In-band checksum status |
| DIAGNOSE_ENDE | `12 04 9F` | Leave session |
| SEED_KEY | `12 08 90 42 4D 57 05 <Z>` | Seed/key init, Z = seconds mod 46 |
| DS2→K2 protocol switch | `12 05 90 56` | SCHALTE_PROTOKOLL_DS2_NACH_K2 — seen live 2026-11-09 |
| FLASH_LESEN | `12 09 06 <A_lo> <A_mid> <A_hi> <count>` | Read flash at address |
| FLASH_LOESCHEN | `12 09 07 06 <A> <count>` | Erase flash |
| FLASH_SCHREIBEN | `12 09 07 02 <A> <data…>` | Write block |
| FLASH_SCHREIBEN_ENDE | `12 09 07 0F <A> <00>` | End write |
| MW live data | `12 05 0B 10 <id_hi> <id_lo>` | Selectable measurement (see tool32_edc15m_export.csv) |
| Fault clear | `12 09 07 0E 42 41 50 00` | "BAP" clear |

## Build

```sh
cargo build --release
```

Produces `target/release/dde3-toolkit`.

## Run

Run without arguments to launch the egui GUI:
```sh
./target/release/dde3-toolkit
```

Or use CLI subcommands:
```sh
# Identify the ECU
./target/release/dde3-toolkit --port /dev/ttyUSB0 ident

# Read full 256 KB flash
./target/release/dde3-toolkit --port /dev/ttyUSB0 read flash.bin

# Read calibration area (0x38000-0x3FFFF)
./target/release/dde3-toolkit --port /dev/ttyUSB0 read-cal cal.bin

# Enter programming mode
./target/release/dde3-toolkit --port /dev/ttyUSB0 uprog-ein

# Seed/key (Z from current time, see HANDOVER.md §15.3)
./target/release/dde3-toolkit --port /dev/ttyUSB0 seed-key 0x05

# Clear faults
./target/release/dde3-toolkit --port /dev/ttyUSB0 fault-clear

# Verify/correct checksums (UNVERIFIED algorithm)
./target/release/dde3-toolkit --port /dev/ttyUSB0 checksum flash.bin

# Full write (requires typing WRITE to confirm)
./target/release/dde3-toolkit --port /dev/ttyUSB0 write flash.bin
```

Options: `--port <path>`, `--baud <rate>`, `--data-delay <ms>`.

## Checksum status

The DDE3 checksum algorithm uses the **verified TDI V3.1 algorithm** ported
from `edc15m_checksum.py` (see `edc15m-checksum-tool/`). The Rust
implementation is self-tested against regression vectors:
- Calibration block: `0x007BC5EB`
- Code block: `0x2477887F`

Block layout: calibration at `0x38000-0x3FFFF` (checksum at `0x7BFC`), code
area at `0x10000-0x14000` (checksum at `0x13FFC`).

⚠️ The block layout has been validated against the Python reference but
**still needs hardware confirmation** against real ECU data before flashing
corrected images.

## Testing

```sh
cargo test
```

All frame builders are unit-tested against the VM-extracted frames in
`analysis/frames.md`. The checksum module is tested for determinism and
idempotent correction.

## Safety

- The ECU must be powered and the engine stopped.
- A write requires a verified full backup and typing `WRITE`.
- Checksums are NOT auto-corrected for DDE3 writes (algorithm unverified).
- Do not interrupt power during erase or programming.

## Relationship to dde4-toolkit

Reference architecture: `EDC15C4-decomp/dde4-toolkit/` (Rust/egui GUI,
~4900 lines). dde3-toolkit mirrors the feature set and code layout
(`protocol.rs`, `checksum.rs`, `main.rs`) but uses DDE3 service codes
(DS2: 0x0D/0x00/0x0A/0x06/0x07/0x0E/0x90/0x9F/0x0B MW) instead of DDE4's
UDS/KWP2000 codes (0x27/0x10/0x31/0x34/0x36/0x23/0x2C/0xA2/0x82).

## Next steps (see HANDOVER.md)

1. **ECU reply frames** — captures so far show only tester TX (what the ECU
   sees); capture ECU responses to confirm reply framing and status bytes,
   and decode the xsetpar timing bytes.
2. **Fault code READ** — no dedicated fault-read job exists; determine if
   any STATUS_* job can read DTCs, or document as clear-only.
3. **Hardware validation** — validate checksum, live data, and write paths
   against real ECU before flashing corrected images.
4. **More unit tests + protocol log enhancements.**

✅ **Completed**: GUI (7 tabs), live data via MW service 0x0B, TDI V3.1
checksum algorithm ported and self-tested, fault clear implemented.
