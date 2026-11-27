# DDE3 Toolkit — Handover

> For the agent taking over. Read this file, then `docs/wire-protocol.md`
> (on-the-wire facts) before touching the protocol code. Completed session
> notes have been dropped — the git history (`git log`) and `docs/` carry
> that detail. This file keeps only what is **open / unverified / pending**.

## Mission

Build a **DDE3 (BMW EDC15 DDE3.0, "M47") K-line diagnostic tool in Rust**
mirroring the `dde4-toolkit` feature set: full/partial flash read & write,
live data, protocol logging, fault read/clear. The protocol was
reverse-engineered from `10DDE30.prg` (WinkFP bytecode) with
`tools/prg_vm.py`. Note: **EDC15M and DDE3 are the same ECU** (Bosch EDC15M
chip, DDE 3.0 hardware name).

## Documentation index

| File | Contents |
|---|---|
| `docs/wire-protocol.md` | **Authoritative** on-the-wire facts: framing, reply convention (`0x12` prefix, not `0x92`), XOR checksum, timing, ECU identity, block sizes, pacing, DS2 UART timing |
| `docs/ds2-rx-buffer.md` | Firmware-confirmed K-line RX buffer & frame limits: 128-byte app receiver (`0x1EB34`), the two-receiver split (why 233-byte MPPS frames work), the unconfirmable "256" (boot block all `0xFF`) |
| `docs/dtc-fault-protocol.md` | 4-step fault-read protocol, 50-byte fault record layout, DTC text tables in `DDE30DS0.prg` |
| `docs/checksum.md` | TDI V3.1 checksum algorithm, block discovery, the false-BAD fix, the cold-start program chain |
| `docs/baud-0x91-patch.md` | DS2 0x91 baud-change ECU patch + `baud` command |
| `docs/patches.md` | Manifest-driven firmware patches (CLI `patches` + GUI tab), builders, prebuilt images |
| `docs/reverse-engineering.md` | .prg format, the 33 jobs, WinkFP VM status, DDE4 parallels, job re-scope |
| `docs/uprog-ein.md` | UPROG_EIN (`0x0D`): the handler decompilation, the 72-byte region/segment table, what it's used for (PRUEFCODE, programming-mode gate, tester region map) |
| `docs/session-log.md` | Historical status snapshots (superseded content) |
| `docs/FINDINGS_*.md`, `docs/SESSION_2026-11-09.md`, `docs/handoff-fast-baud.md` | Earlier finding write-ups |

## Current status (v0.5.2, 2026-11-27)

Shipped and verified (detail in git history + `docs/`):

- ✅ Wire framing (`<DS2 payload> <XOR>`, 9600 8E1, `0x12` reply prefix)
  confirmed by hardware capture; LEN-validated `FrameParser` + echo-wait
  retransmit (fake-ECU E2E).
- ✅ Full flash read with flasher-session bootstrap (IDENT → UPROG_EIN → AIF
  → SEED_KEY), starting at 0x8000 (IROM is never exposed — §36);
  two-session full write; MPPS-style calibration write (224-B blocks,
  `07 0F` @0x03FCE0, DTC prep) — the cal write ran to completion on the real
  car once (car started afterwards).
- ✅ Fault read/clear, DTC tables, TDI V3.1 checksum + program chain,
  manifest-driven patches, 8-tab GUI, `respond` fake ECU (Galletto-parity,
  `--flash`/`--dump`/`--patched-baud`), headless `bench`, live/logger with
  presets + four Live view modes, signed (i16) ADF decode.
- ✅ `cargo test` → 99 passing (95 unit + 4 fake-ECU E2E), clippy clean.
  Last release **v0.5.2** (linux + windows x86_64, `dist/`).
- ⬜ Open: the on-car verifications below.

**E2E fake-ECU tests** (`tests/e2e_fake_ecu.rs`): full read, cal read,
full write, cal write — the real CLI against `respond` over a socat pty
bridge, image round-trips byte-compared. **Not definitive** (fake ECU) and
they need a real 256 KB dump at `captures/actual_flash.bin` (private —
never committed; supply your own) plus `socat`.

## Open / unverified items

### 1. Full read from 0x8000 — fake-ECU E2E verified, on-car run optional
The ECU does not expose its IROM: `[0, 0x8000)` always reads FF, so
`read_full` reads `[0x8000, 0x40000)` (1792 × 128 B chunks instead of 2048,
~12.5 % less read time) and FF-pads the prefix — output layout unchanged
(wire-protocol §36). **Fake-ECU E2E verified 2026-11-27**
(`captures/26-11-2026 test full read speedup/`): 1792 chunks from 0x008000,
all steps 0x80, 208 ms/chunk (same as before), chunk-loop span **371.9 s vs
425.0 s** for the old 2048-chunk read (53 s / 12.5 % saved); output
byte-identical to the served flash. An on-car run is optional — the FF-
prefix premise is already proven by the real-car dump
(`captures/actual_flash.bin`).

### 2. MPPS-style cal write with the session-46 pacing fix — on-car re-verification
`write-cal` must show the bootstrap IDENT **succeeding on the first try**
(`TX 12 04 00 16` → `RX 12 2E A0 …`, no "IDENT no reply" line) and data
frames at ~327 ms/frame (MPPS parity; 3 ms/byte only around the bootstrap,
0 around the data-frame loop — wire-protocol §35.5–35.6). Check with
`tools/analyze_realcar_write.py`.

### 3. Two-session full write at 224-byte blocks — on-car verification (NEW EXPERIMENT)
`write_full` now uses the calibration-write block size in **both** regions
(731×224 + 1×96 program, 146×224 + 1×64 cal = 879 frames, down from 3072 ×
64 B — wire-protocol §37). 224 B is on-car-proven only for the calibration
area; the program-area use is a deliberate new experiment (distinct from
the rejected 192 B, §32). `dde3-toolkit write <backup>.bin` (destructive —
do it last, with a verified backup in hand) must show: program area
0x10000..0x38000 → DIAGNOSE_ENDE → re-auth (second UPROG_EIN + SEED_KEY) →
erase @0x38000 → calibration 0x38000..0x40000 → DIAGNOSE_ENDE; 879 ×
224-byte writes, no `07 0F`. **If the ECU hangs at 0x10000 like the 192 B
attempt, revert `write_full` to `WRITE_BLOCK` (64 B)** and re-verify with
the old size.

### 4. Rewritten `read_dtcs()` — on-car confirmation
`dde3-toolkit dtc --port …` → expect **"Gluehanlage"** (ORT 0x08) + ART
texts "Gluehkerzen bzw. Gluehrelais defekt" / "Fehler momentan vorhanden" /
"Fehler sporadisch" — **not** "Mengensteller". Also check the wire log for
the step-4 tail-count reply shape (`12 05 14 01` again → `<anz'> <rec
37..50> [<next header>]`); if it differs from `docs/dtc-fault-protocol.md`,
adjust only the tail slice in `read_dtcs()`.

### 5. 0x91 fast-baud patch — on-car half
The fake-ECU half is done (`respond --patched-baud`, pty E2E). On-car:
flash `e46_baud125000.bin`, `dde3-toolkit baud --rate 125000`, confirm
IDENT at 124800, then the power-cycle revert check
(`docs/baud-0x91-patch.md`).

### 6. Live-loop TX→RX gap measurement
Real-car baseline was 2.50 Hz (steady 399 ms/cycle, 5 ids) with ~250 ms
unexplained. The next live capture must read the TX→RX gap to see whether
the ECU itself is the bottleneck and what it does when polled back-to-back.

### 7. MW live-data ADF ids
The ADF table comes from the EDC15M export; ids to be confirmed on the real
ECU (battery 0x0F65 already confirmed).

### 8. Fake-ECU MPPS session
Re-run MPPS against `respond --flash captures/actual_flash.bin` and confirm
the VIN read + read/write buttons gate pass.

### 9. Windows 10/11 build — native-machine test
Branch `feature/windows-10-11-build` (based on the full-read branch):
`dist/dde3-toolkit-windows-x86_64.exe` (xwin/MSVC, no Win7 delay-load
hacks). Wine check done — the GUI window comes up
(`WINEPREFIX=/home/ilija/Bottles/Programs/ wine`). Pending: run it on a
native Win10/11 machine.

## Open branches

- `feature/full-write-224-blocks` — item 3: full write at 224-byte blocks
  (calibration-write size) + the E2E fake-ECU test suite
  (`tests/e2e_fake_ecu.rs`). Merge to main when the on-car write passes.

## Closed — do not re-attempt without new evidence

- **192-byte block mode** (session 33): rejected on-car — the ECU OKAYed
  two 192-B write blocks, then hung. 64 B writes / 128 B reads (and 224 B
  for the calibration area, proven by the MPPS capture) are the only
  verified sizes (wire-protocol §32–33). Note: the in-flight 224-B **full
  write** (item 3, §37) is a deliberate new experiment — 224 B ≠ 192 B —
  but it shares the same failure mode to watch for (hang at 0x10000).
- **Per-zone `07 0F` write-end failsafe** (session 31): not implemented —
  the job decompilation doesn't determine the address field or the call
  timing. Rule: don't implement a command the decompilation doesn't fully
  explain. A real MPPS/PC-software capture showing a `07 0F` frame resolves
  both open questions.
