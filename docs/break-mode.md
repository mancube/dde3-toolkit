# Break-entered service protocol (10.4 k / 38.4 k)

The secondary, break-entered service protocol that carries the
byte-addressed RAM/EEPROM/PEC access on the DDE3 K-line.

## Session 10 — DS2 baud analysis, batched live data, break-entered service protocol

### What was asked
Confirm the ECU's DS2 handling for the *fastest* read/write/logging, batch
live data into one telegram (as other loggers do), and plan + implement the
break-entered service protocol.

### Findings (full write-up: `FINDINGS_baud_service_protocol.md`)
- The K-line is one C166 S0 UART with a **3-value baud ladder**: `S0BG` =
  `0x33` (9600, default), `0x2F` (~10.4 k), `0x0C` (~38.4 k).
- **The DS2 diagnostic protocol is fixed at 9600** — the receive state machine
  at `0x1E904` (states 0–2) never re-rates the port, and there is no in-band
  DS2 command that does. DS2 read/write/log ceiling = 9600 8E1.
- **DS2 length-byte rule** (receiver `0x1EB2C`/`0x1EB46`): the length byte
  equals the **total wire byte count** (address byte + payload + trailing XOR),
  capped at 0x80.
- The **tested** WinKFP logger (`testo customjobs.txt`, job
  `MW_SELECT_LESEN_NORM`, disasm `0x5FE5F`) sends the whole live set in ONE
  telegram: `12 <LEN> 0B 10 <ids…>` with `LEN = 5 + 2·n` (the job computes it
  with `slen + 5`).
- **The bug:** `frame_mw_select` hard-coded `LEN = 0x05` and the loop chunked
  in 10s. The sim ignored the length byte, so it only worked against the sim.
- The 10.4 k / 38.4 k bauds live in a **separate break-entered service
  protocol** (states 3–6 of `0x1E904`): UART break → 10.4 k → handshake
  (`0xA5`, gated on internal unlock flags `C0DF.0`/`C3A9==0xA5`) → optional
  38.4 k. `BAUDRATEN_*` jobs target the CAN/EDIC bus, not the K-line.
- **Open (needs HW):** the secondary command opcodes at `0x1F6D2` (the
  byte-addressed RAM/EEPROM/PEC interpreter) — not decoded, not guessed.
  Also flagged: the flash read/write frames use a 2-byte count in the reference
  (`12 09 06 <A:3> <n:2>`) vs the toolkit's 1-byte count + hard-coded `LEN=0x09`;
  left unchanged pending a capture (a full 256 KB write reportedly worked on HW,
  so the framing model for that path is unresolved — do NOT change without a capture).

### Changes
- **`src/protocol.rs`**
  - `frame_mw_select` now computes `LEN = 5 + 2·n` (with a `MW_BATCH_SIZE` = 50
    guard, under the 0x80 cap) instead of hard-coding 0x05.
  - Added raw transport for break mode: `Kline::raw_write` / `raw_read` /
    `raw_break` (uses `serialport` `set_break`/`clear_break`).
- **`src/app.rs`** — the live loop now sends the selected set in as few
  telegrams as the cap allows (one per `MW_BATCH_SIZE`) instead of one per 10;
  dropped the dead `defined`/`single`/`redefine` scaffolding.
- **`src/breakmode.rs`** (new) — the break-entered service protocol: `s0bg`
  baud constants, `enter(kline, fast)` (break → 10.4 k → `0xA5` handshake →
  optional 38.4 k), `BreakHandle` raw transport + `exit()`, `BreakSim`, unit
  tests. CLI: `dde3-toolkit break-probe [--fast]`.
- **Tests** — new `mw_select_batched_length` (protocol) and `mw_live_batch`
  (sim) lock in the batched framing; `breakmode` baud-ladder + handshake-gate
  tests. **33 passing** (release + debug).

### Next step (hardware)
Run `dde3-toolkit break-probe --port /dev/ttyUSB1` (after UPROG_EIN + seed/key)
with `tools/monitor.py` capturing `/dev/ttyUSB0`, to (a) confirm the handshake
byte/rate that selects 38.4 k, and (b) capture the secondary command opcodes at
`0x1F6D2`. Then add the byte-addressed read/write commands to `breakmode.rs`.


## 20. Session 11 — break-mode secondary command set DECODED + implemented (offline RE)

With no ECU on the bench, the `0x1F6D2` interpreter was decoded fully from
`/home/ilija/Desktop/edc15m_out/research/out/disasm.txt` (ground truth) and
cross-checked against `research/routines_part2.md`.

### What is now known (and implemented in `src/breakmode.rs`)
- **Frame shape**: raw 4-byte blocks, no DS2 length/XOR —
  request `[0x01, OPC, xx, DATA]` (first byte MUST be 0x01, gate `0x1F6EC`),
  reply `[0x00, TAG, RESP, HI]` (TAG 0x02/0x01/0x05, HI 0x01 data / 0x00 default).
- **Opcode map** (slot = `(table_addr − 0x1F724)/4`, 0x51 entries, clamped to 0x50):
  0x02 echo+checksum, 0x04/0x07 addr low/high, 0x0B/0x0E/0x0D word-aware RAM
  reads, 0x10 block-buffer read, 0x13 fault/DTC read (`+0xCE7A`), 0x18 EEPROM
  16-bit read, 0x1B checksum, 0x25/0x26 set addr, 0x2A block handshake/reset
  (`DATA` must be 0x11), 0x31 block store, 0x3E PEC bulk, 0x4F set block size.
- **Shared-register subtlety**: the address register is the 24-bit value at
  `C804..C806` and the **partner cache** is the 24-bit value at `C806..C808` —
  they **share byte C806**; the word-read path stores `C806..C808 ← addr −
  0x0FF1E` before the 24-bit `C804 −= 1` decrement. `BreakSim` models this with
  a 5-byte shared block.
- **Two unknown-opcode defaults**: `0xED` (slot A, `0x1FBBA`) / `0xEB` (slot B,
  `0x1FBD0`), `HI = 0x00`; per-slot A/B transcribed in `BreakSim::default_reply`.
- **Corrections to routines_part2.md**: checksum is **0x1B** (not 0x1C),
  16-bit EEPROM read is **0x18** (not 0x19).

### Changes
- **`src/breakmode.rs`** — added `opcode::*` constants, `request()` frame builder,
  `Reply`/`parse_reply()`, `BreakHandle::command()` + typed helpers
  (`reset_block`, `set_addr`, `get_addr`, `checksum`, `read_ram`), and the
  mailbox-faithful `BreakSim` (RAM + block buffer + checksum + addr/partner
  registers + default replies). `enter()` now sends the **second** handshake
  byte (`0xA6` = fast → 38.4 k latch, else `0xA5` = stay 10.4 k).
- **`src/main.rs`** — `break-probe` drives the verified sequence
  (`0x2A/0x11`, `0x1B`, `0x04/0x07`, legacy `0x51` → expected `00 02 ED 00`).
- **FINDINGS §4.4** (new) — the decoded command format + opcode table + the
  corrections above.
- **Tests** — 12 `breakmode` unit tests (was 2). **43 passing** (release+debug),
  build clean (no breakmode warnings).

### Next step (hardware, when the ECU returns)
1. **T2.1 capture**: `break-probe` + `tools/monitor.py` on `/dev/ttyUSB0` to
   confirm the on-wire 4-byte layout (byte order, echo behaviour, PEC framing).
2. Then wire the byte-addressed RAM/EEPROM/fault reads into the CLI/GUI and the
   `0x3E` PEC bulk stream (the only piece still capture-pending).

