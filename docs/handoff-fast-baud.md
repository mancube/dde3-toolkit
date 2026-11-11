# Handoff — DDE3.0 / EDC15M fast-baud work

> **Purpose:** hand this file to the next agent so it can continue without
> re-deriving anything. Everything below is grounded in the reverse-engineering
> artifacts and the current `dde3-toolkit` code. Read this first, then
> `FINDINGS_baud_service_protocol.md` (the deep dive), then `HANDOVER.md`.

---

## 1. The goal

Get the **fastest possible** read / write / data-logging rates to the ECU, and
get `dde3-toolkit` (Rust CLI + egui GUI) to work with those settings.

**Bottom line from the RE (do not re-derive):**
- The K-line is one C166 S0 UART. Baud is set by the `S0BG` register and has
  only three values in the firmware: `0x33` = **9600** (default), `0x2F` ≈
  **10.4 k**, `0x0C` ≈ **38.4 k**.
- **The DS2 diagnostic protocol is locked at 9600 8E1.** The receive state
  machine at disasm `0x1E904` (states 0–2) never re-rates the port; no DS2
  service changes baud. So 9600 is the ceiling for all DS2 flash read/write and
  `0B` live-data. `dde3-toolkit` already runs there — it is *not* slow for a
  fixable reason at the baud level.
- The only faster link is the **break-entered service protocol** (10.4 k /
  38.4 k), which is a *different, byte-oriented protocol* entered by a UART
  break, not a DS2 speed-up.
- The real, immediate win was **fewer round-trips**, which is done: live data is
  now batched into one telegram per `MW_BATCH_SIZE` (see §4).

## 2. State of the code (what already works)

Build: `cd dde3-toolkit && cargo build --release` — clean.
Tests: `cargo test --release` — **33 passing** (debug + release).

Already implemented and committed this session:
- **Batched live data** (`src/protocol.rs::frame_mw_select`, `src/app.rs`):
  selected live set sent in as few `0B 10` telegrams as the 0x80-byte DS2 cap
  allows. `LEN = 5 + 2·n`. Matches the **tested** WinKFP logger
  (`testo customjobs.txt` → job `MW_SELECT_LESEN_NORM`, disasm `0x5FE5F`).
  Lock-in tests: `protocol::tests::mw_select_batched_length`,
  `sim::tests::mw_live_batch`.
- **`src/breakmode.rs`** (new module) + CLI **`dde3-toolkit break-probe [--fast]`**:
  - `s0bg` constants, `enter(kline, fast)` = break → 10.4 k → `0xA5` handshake →
    optional 38.4 k; `BreakHandle` raw byte transport + `exit()`; `BreakSim`.
  - `Kline::raw_write` / `raw_read` / `raw_break` in `protocol.rs`
    (real UART break via serialport `set_break`/`clear_break`).
  - **Entry handshake is implemented and unit-tested; the secondary command
    opcodes are NOT** (see §3, task T2).
- Docs: `FINDINGS_baud_service_protocol.md`, README + HANDOVER (Session 10).

## 3. What is NEXT to be done (the task list for you)

### T1 — Verify the batched live path on real hardware (PRIORITY)
The length-byte fix is grounded but **only sim-verified** so far.
1. Unlock the ECU first: UPROG_EIN + seed/key (`dde3-toolkit uprog-ein`,
   `dde3-toolkit seed-key <Z>`, Z = seconds mod 46).
2. From the GUI Live tab (or a CLI probe) read a multi-id set, e.g. the tested
   `SET_3`: `0F63 0F62 0F61 0F60 0F65 0F00 0F30 0F10 0E81 0F80 0F8A 0F84 0F52
   0F53 0F40`. Confirm the ECU returns `92 05 0B 10 A0 <u16 BE per id>` with
   the right number of values.
3. Capture the wire with `tools/monitor.py` on the tap port and confirm the
   on-wire `LEN` byte equals the total byte count (rule: `FINDINGS` §2.1).
   **If the ECU NACKs the batched frame, the `LEN` rule is wrong — re-derive
   from the capture before touching anything else.**

### T2 — Decode + implement the secondary (break) protocol commands
This is the actual path to 10.4 k / 38.4 k bulk access, and it is **not yet
decoded**. The entry handshake is done; the command set is the gap.
1. On hardware (after T1 unlock): `dde3-toolkit break-probe --port /dev/ttyUSB1`
   (and `--fast`) while `tools/monitor.py` taps `/dev/ttyUSB0`. The current probe
   sends byte `0x51` and dumps whatever the ECU answers.
2. Confirm the exact **handshake word that selects 38.4 k** (state 6 checks
   `C332 & 0xFF00 == 0xA600`, disasm `0x1ED4C`). The current code sends `0xA5`;
   the high byte that latches `0xA6` may differ — capture it, don't assume.
3. Read the secondary interpreter at disasm **`0x1F6D2`** (the `0x51`-entry
   jump table) and its RAM/EEPROM/PEC bulk paths (routines part 2 §5). Map the
   opcodes for: read RAM, read EEPROM/flash, write, PEC bulk.
4. Add the resulting command frames to `breakmode.rs` (a `BreakHandle` method per
   operation, mirroring `Kline::read_range`/`write_block`), with a sim + tests.
5. Then wire a `break` read/write mode into the CLI/GUI so the 38.4 k link can
   actually move data.

**Guardrail:** the break entry requires the DS2 session to be unlocked first
(internal flags `C0DF.0` set, `C3A9 == 0xA5`, disasm `0x1EC70`–`0x1EC90`). If
`break-probe` fails the handshake, the ECU wasn't unlocked — do UPROG_EIN +
seed/key first.

### T3 — Resolve the flash read/write framing question (do NOT change blind)
`FINDINGS` §2.4 flags a real contradiction:
- The VM ground truth (`tools/analysis/frames.md`) shows flash frames as
  `12 09 06 <A:3> <n:2>` (2-byte count, `LEN=0x09` for the minimal form).
- The toolkit's `frame_flash_read`/`frame_flash_write` use a **1-byte** count and
  a **hard-coded `LEN=0x09`**, which violates the §2.1 length rule for a
  0x80-byte block (would exceed the 0x80 cap).
- **Yet** a full 256 KB write reportedly completed on HW (HANDOVER §18.1).
**Action:** capture a real flash-read and flash-write session and compare.
Either the ECU is lenient about `LEN` (keep as-is) or the toolkit framing is
wrong (fix `frame_flash_read`/`frame_flash_write` + the `sim.rs` flash handler
together). Do not change this path from inference alone.

### T4 — (Nice-to-have) speed levers at 9600, once T1/T3 are solid
- Minimize `--data-delay` toward the ECU's real inter-byte floor (the ECU waits
  `SCALARS@0x2EF4` before retransmit; the ~50 ms WinKFP gap is tester-side, not
  required). The practical floor is "next request the instant the reply's
  trailing XOR lands" — check `Kline::read_chunk`/`silence` for headroom.
- For the fixed measurement block, `frame_mw_status()` (`0B 03`) returns 25
  values in one telegram — consider using it when the watched set fits one group.
- Consider raising `MAX_FRAME` (currently 64) if a legitimate DS2 data reply
  (e.g. flash read) can exceed it — check against a capture before bumping.

## 4. Critical references (do not re-search these)

| Thing | Where |
|---|---|
| Findings deep-dive (baud, DS2, break) | `FINDINGS_baud_service_protocol.md` |
| Full session history + HW notes | `HANDOVER.md` (Session 10 = this work) |
| Disassembly (S0 ISR `0x1E904`, states 3–6 `0x1EC06`–`0x1ED96`, DS2 dispatch `0x290D4`, secondary `0x1F6D2`) | `edc15m_out/research/out/disasm.txt` |
| DS2 command map | `edc15m_out/research/routines_part3.md` |
| Secondary-protocol routines | `edc15m_out/research/routines_part2.md` |
| Tested logging jobs (ground truth) | `testo customjobs.txt` |
| VM-extracted frame ground truth | `dde3-toolkit/analysis/frames.md`, `ds_frames.md` |
| Live variable table (81 ADF) | `dde3-toolkit/src/live.rs` |
| Protocol / frames / Kline | `dde3-toolkit/src/protocol.rs` |
| Break mode | `dde3-toolkit/src/breakmode.rs` |
| GUI live loop | `dde3-toolkit/src/app.rs` (`start_live`) |
| In-process ECU sim | `dde3-toolkit/src/sim.rs` |
| Passive K-line sniffer | `dde3-toolkit/tools/monitor.py` |
| A2L (1629 chars) | `edc15m_out/EDC15M_7789659.a2l` |

## 5. Environment / hardware

- **Sender** (drives K-line): `/dev/ttyUSB1` (FT232R). **Passive tap** (ECU-side
  view, cannot drive): `/dev/ttyUSB0` (UART-LPT). The sim injects replies
  because the tap can't drive the bus — so **sim tests do NOT prove HW framing**;
  that is exactly what T1/T3 must confirm on the wire.
- ECU: EDC15M-6.1 / DDE3.0 (BMW E46 M47, Bosch `0 281 010 205`, prog 7786887).
  Address `0x12`. Flash base `0x0`, 256 KB; calibration `0x38000`, 32 KB.
- Seed/key: `12 08 90 42 4D 57 05 <Z>`, `Z = seconds mod 46`.
- Build/test: `cargo build --release` / `cargo test --release` in `dde3-toolkit`.

## 6. Do / don't
- **Do** capture before changing any wire framing. The sim is lenient and has
  already masked at least one framing bug (the batched `LEN`).
- **Don't** implement secondary-protocol opcodes from guesswork — capture them
  (T2).
- **Don't** touch the flash read/write framing (T3) without a capture; a full
  write is on record working, so the model is genuinely unresolved.
- **Do** keep the length-byte rule (`FINDINGS` §2.1) and the tested-logger
  reference (`testo customjobs.txt`) as the check for any new frame builder.
