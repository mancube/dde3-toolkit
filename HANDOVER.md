# DDE3 Toolkit — Handover

> For the agent taking over. Read this file, then `docs/wire-protocol.md`
> (on-the-wire facts) before touching the protocol code.

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
| `docs/wire-protocol.md` | **Authoritative** on-the-wire facts: framing, reply convention (`0x12` prefix, not `0x92`), XOR checksum, timing, ECU identity |
| `docs/dtc-fault-protocol.md` | 4-step fault-read protocol, 50-byte fault record layout, DTC text tables in `DDE30DS0.prg` |
| `docs/checksum.md` | TDI V3.1 checksum algorithm, block discovery, the false-BAD fix |
| `docs/break-mode.md` | Break-entered service protocol (10.4 k / 38.4 k) + decoded command set |
| `docs/baud-0x91-patch.md` | DS2 0x91 baud-change ECU patch + `baud` command |
| `docs/reverse-engineering.md` | .prg format, the 33 jobs, WinkFP VM status, DDE4 parallels, job re-scope |
| `docs/session-log.md` | Historical status snapshots (superseded content) |
| `docs/FINDINGS_*.md`, `docs/SESSION_2026-11-09.md`, `docs/handoff-fast-baud.md` | Earlier finding write-ups |

## Current status (2026-11-11)

- ✅ Wire framing confirmed by hardware capture (no B8 envelope:
  `<DS2 payload> <XOR>`, 9600 8E1).
- ✅ Real-car reply convention fixed: ECU replies with prefix **`0x12`**,
  not `0x92` (5 new real-car fixture tests, `cargo test` → 48 passing).
- ✅ Fault read/clear, DTC tables (embedded in `DDE30DS0.prg`), checksum
  (TDI V3.1, byte-for-byte parity with the Python reference), 8-tab GUI,
  headless bench, in-process ECU sim.
- ⬜ Open: real-car verification of the items below.

## ← CURRENT TASK (take over from here)

**Real-car verification of the rewritten `read_dtcs()` (session 12).**
`read_dtcs()` was rewritten for the 4-step chunked fault protocol
(`docs/dtc-fault-protocol.md`); the old version mis-parsed the ORT field.
One real-car confirmation is still pending. Steps:

1. `dde3-toolkit dtc --port /dev/ttyUSB1` → expect **"Gluehanlage"** (ORT
   0x08) + ART texts "Gluehkerzen bzw. Gluehrelais defekt" /
   "Fehler momentan vorhanden" / "Fehler sporadisch" — **not**
   "Mengensteller".
2. Check the wire log (`captures/dde3newlog.txt`) for the **step-4 tail-count reply
   shape** (`12 05 14 01` again → `<anz'> <rec 37..50> [<next header>]`).
   If it differs from the table in `docs/dtc-fault-protocol.md`, adjust only
   the tail slice in `read_dtcs()` — the rest of the pipeline is
   data-driven.

### Fast-logger status (2026-11-11)

Tier 0 of `docs/FINDINGS_faster_logger.md` is implemented: 55 ms poll sleep
removed, live TX pacing 3→1 ms/byte (flash-write pacing untouched), and the
protocol log is now timestamped (`[+Δs]` gap per line). Real-car baseline was
2.50 Hz (steady 399 ms/cycle, 5 ids) with ~250 ms unexplained — **next live
capture must read the TX→RX gap** to see whether the ECU itself is the
bottleneck. Expected ≥3.2 Hz after this change; if ECU latency dominates, the only
remaining lever is Tier 2 (38.4 k break channel) — the ECU transmits
nothing spontaneously (2026-11-11 passive capture), so no listen mode.

### Session 13 (2026-11-12) — flash framing resolved, fast-DS2 patch corrected

New: `docs/FINDINGS_flash_and_fast_ds2.md` (authoritative). Key results:
- **DS2 dispatcher fully mapped** (disasm 0x2932C–0x293D0). Services 0x06/0x07
  are **NACK stubs (status 0xA2) in the normal session** — real flash read/write
  only works in programming mode (after `UPROG_EIN` `12 04 0D`), handled by the
  boot block (not in any dump).
- **Flash framing RESOLVED and FIXED** (was the "T3 contradiction"). Running the
  WinKFP job VM with an over-range count probe + the job bytecode prove: read =
  `12 09 06 00 <addr BE> <count:1>`, write = `12 <9+N> 07 02 <addr BE> <N> <data>`,
  erase/end `07 06`/`07 0F <addr BE> 00`. The old `frame_flash_read` was
  little-endian, dropped the `[3]=00` byte and kept LEN=0x09 for a 7-byte payload,
  so the real car never replied (`captures/dde3newlog.txt`). `frame_flash_read/
  _write/_erase/_write_end` + the `sim.rs` decoders + tests are now corrected.
  Adversarially reviewed (4 dimensions) — compiles clean, no blockers.
- **Checksum-status reply captured**: `12 04 0A` → `12 05 A0 00`
  (`captures/dde3newlog.txt`). Open item #1 below is now closed.
- **Fast DS2 is real and patched.** `../../patches/patch_baud.py` adds DS2
  service 0x91 and switches S0BG to 38400 (0x0C) or **125000 (0x03 → 124800)**.
  The old `baud91_patch/` draft was wrong twice (missed the cold-start
  program-chain checksum → reset loop; used JMPA for a cross-segment jump →
  crash); the new builder fixes both and ships checksum-valid images
  `e46_baud38400.bin` / `e46_baud125000.bin`.
- **Toolkit:** `baud` takes `--rate 38400|125000` (reopens at the ECU's actual
  divisor, 125000→124800); `bench --fast-baud <rate> --live` is a headless fast
  logger; the GUI Live tab has a "Fast baud (125k)" toggle (link label + write
  pacing follow the active rate, state resets on disconnect/link-loss). All
  adversarially reviewed across compile/protocol/test/integration/egui
  dimensions — no blockers. Build on the host (`cargo build --release &&
  cargo test`; no Rust on the analysis machine).

NEXT (hardware): flash `e46_baud125000.bin` on a bench ECU, `dde3-toolkit baud
--rate 125000`, confirm IDENT at 124800; then the batched live loop logs at
~30–50 Hz (vs 2.5 Hz at 9600). Also capture one `UPROG_EIN`+FLASH_LESEN to pin
the flash read count-width before touching `frame_flash_read`.

### Remaining open items (after the above)

1. ~~Checksum hardware confirmation~~ — **done**: `12 04 0A` → `12 05 A0 00`
   captured 2026-11-10 (`captures/dde3newlog.txt`).
2. **MW live-data ADF ids** — the ADF table comes from the EDC15M export;
   ids to be confirmed on the real ECU (battery 0x0F65 already confirmed).
3. **Break-mode T2.1 capture** — `break-probe` + `tools/monitor.py` on
   `/dev/ttyUSB0` to confirm the on-wire 4-byte layout, then wire the
   byte-addressed reads into CLI/GUI (`docs/break-mode.md`).
4. **0x91 patch bench test** — flash `e46_baud91.bin`, `baud` subcommand,
   power-cycle revert check (`docs/baud-0x91-patch.md`).
