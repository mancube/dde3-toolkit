# Faster logging on the DDE3.0 / EDC15M — findings and plan

Companion to `FINDINGS_baud_service_protocol.md` (baud ladder, DS2 framing,
break-protocol decode) and `handoff-fast-baud.md` (task list T1–T4).
This file answers one question: **what gets the logger faster, in what order,
and without firmware modification?**

All addresses are C166 program memory (`edc15m_out/research/out/disasm.txt`).

---

## 1. The facts that constrain the answer

1. **DS2 is hard-locked at 9600 8E1.** The S0 receive state machine
   (`0x1E904`) never touches `S0BG` in states 0–2, and no DS2 service re-rates
   the port. 9600 is the absolute ceiling for `0B 10` live data, flash, etc.
2. **38.4 k exists in stock firmware, unlocked by hardware events, not by any
   DS2 command.** Entry is a UART **break** while DS2 is idle, then a two-byte
   handshake. No firmware patch is required to use it — it is the factory
   service channel.
3. **38.4 k is a different protocol** (raw 4-byte frames, the `0x1F6D2`
   interpreter), not "DS2 at higher speed". Its RAM/EEPROM bulk reads are
   exactly what a fast logger can exploit (see §3, Tier 2).
4. **Today's logging rate is capped by tester-side pacing, not by the wire.**
   The live loop sleeps 55 ms between polls and TX is paced 3 ms/byte — both
   inherited from WinKFP habits, neither required by the ECU.

## 2. The 38.4 k mode, precisely (stock firmware, no modification)

| State | Addr | Action |
|---|---|---|
| →3 | `0x1EA44` | UART break (frame-error condition) while DS2 idle sets `C749 = 3`. |
| 3 | `0x1EC06` | `S0BG = 0x2F` (~10.4 k), `S0CON = 0xD4`, → state 4. |
| 4 | `0x1EC42` | Waits for a byte with high nibble `0xA` (the **`0xA5`** handshake). Accepted **only** when the unlock flags hold: `C0DF.0` set **and** `C3A9 == 0xA5` (`0x1EC70`–`0x1EC90`). Otherwise the UART is reset to the DS2 state. |
| 5 | `0x1ECB6` | Second handshake byte latched into `C332h`, echoed back, → state 6. |
| 6 | `0x1ED4C` | If the latched byte is **`0xA6`** (`C332 & 0xFF00 == 0xA600`) → `S0BG = 0x0C` = **~38.4 k**; any other byte keeps 10.4 k. Then the secondary interpreter (`0x1F6D2`) owns the link. |

Precondition notes (traced this session):

- `C0DF.0` is snapshotted at startup (`0x117DA`) from `C0EA.0`, which is set
  when `FDD0.6` is clear — i.e. **normal (non-factory) vehicles have the flag
  satisfied at boot**.
- `C3A9` is loaded at startup from RAM `C546` (`0x11812`); `C546` is written
  **only** by the boot block (`0x0000–0x7FFF`), which is missing from the
  flash read. Whether `C3A9 == 0xA5` at boot therefore **cannot be confirmed
  statically** — the boot block is the one blind spot. The intended flow (per
  the guardrail in `handoff-fast-baud.md`) is that a prior DS2 unlock
  (UPROG_EIN + seed/key) establishes the session state the state-4 gate
  expects. **This is what the T2 hardware capture must confirm first.**
- `FDD0.6` (the factory-monitor / test-frequency gate) is set at
  `0x1124C`/`0x1272E` from `R2.15` pin logic during startup; a normal car
  leaves it clear.

Baud formula: `baud = UCLK / (16·(S0BG+1))` with UCLK = 8 MHz.
`0x33`→9600, `0x2F`→~10.4 k, `0x0C`→~38.4 k.

## 3. The plan, by tier

### Tier 0 — Remove the artificial pacing (code-only; do first)

No wire-format change, no HW risk beyond a capture check.

1. **`src/app.rs:1234` — the hardcoded `sleep(55 ms)`** between live polls.
   It mimics WinKFP's ~50 ms tester gap. Replace with "send the next request
   the instant the previous reply's trailing XOR has been parsed". Frame
   delimiting already uses 10 ms line silence (`set_silence(10)`), which is
   far above the ECU's real inter-byte gaps; the ECU's own reply latency comes
   from its 0x2EF4 task calibration (few ms), not from any tool requirement.
2. **`--data-delay 3` (ms/byte) on TX** (`protocol.rs:593`, `write_delay`).
   A 35-byte live request costs ~105 ms of pure pacing at the default.
   `HANDOVER` §"unverified" flags `--data-delay 0` as sim-only — verify at
   1 ms on hardware under `tools/monitor.py`, then 0. The ECU RX is a
   task-driven ISR, not bit-timed; continuous streams at 9600 are expected to
   be fine.
3. **Right-size the id set.** 9600 8E1 ≈ 1.14 ms/byte. Expected steady-state:

   | ids/telegram | per-cycle (wire + ECU) | achievable |
   |---|---|---|
   | 15 (SET_3) | ~90–100 ms | **~10 Hz** |
   | 5 | ~33 ms | ~30 Hz |
   | 1 | ~25 ms | ~40 Hz |

   (Today's real rate ≈ 6–7 Hz: the 55 ms sleep *plus* paced TX on top of
   wire time.)

### Tier 1 — Verify the batched live frame on HW (gate for everything)

`LEN = 5 + 2·n` (`FINDINGS_baud_service_protocol.md` §2.1) is sim-verified
only. One `tools/monitor.py` capture of a multi-id `0B 10` request answers
whether the real ECU counts the length byte as total-wire-bytes. **If it
NACKs, the length rule is wrong and Tiers 0/2 numbers above are void until
re-derived from the capture.** (This is handoff task T1.)

### Tier 2 — The 38.4 k break channel as the logger (the real speed-up)

1. **Verify entry** (handoff task T2): unlock the ECU (UPROG_EIN + seed/key),
   then `dde3-toolkit break-probe --port /dev/ttyUSB1 --fast` under the
   monitor tap. Confirm the `0xA5` → `0xA6` handshake, the ECU's echo, and
   that the link actually moves at 38.4 k (byte-timing in the capture).
   Capture — don't assume — the exact second handshake byte that latches
   `0xA6` into `C332`.
2. **Log by direct RAM read, bypassing DS2.** The secondary interpreter
   (`0x1F6D2`, opcode map in `FINDINGS_baud_service_protocol.md` §4.4) does
   24-bit RAM word reads (`0x0B`/`0x0E`) in 4-byte frames at 38.4 k. The
   `0x0F63`-style measurement ids map to RAM addresses via the A2L/cal data;
   scale with the existing `FACT_A/FACT_B`. A 15-value snapshot then costs
   ~25 ms of wire → **~30–40 Hz for the full watched set**, 4× the DS2
   ceiling. `0x3E` PEC bulk covers multi-kB region snapshots.
3. **Physical ceiling:** the ECU's own engine/measurement loop (order
   5–20 ms) caps freshness — expect ~50–100 Hz max per value no matter the
   baud. Past that, more wire speed buys nothing.

### Tier 3 — Firmware patch to run DS2 at 38.4 k (feasible, rejected)

Patching the `S0BG` immediates (`0x1E8F4` et al., `0x33`→`0x0C`) is possible
with `edc15m-checksum-tool`, but is not worth it:

- the DDE30 tester side is hard-locked to 9600 for DS2;
- the ECU's reply-timing scalars (`0x2EF4`: 2/20/4/25/500/40/3000/500 task
  periods ≈ ×10.4 µs) were calibrated around 9600-era behavior;
- every patched image carries a checksum/variant mismatch risk for future
  reads/writes;
- Tier 2 reaches the same 38.4 k legitimately.

## 4. Recommended order and expected outcome

1. **Tier 0** pacing changes — sim-test first, 15-min HW check. → ~10 Hz
   full-set from ~6.7 Hz.
2. **T1** batch-verification capture (prerequisite gate).
3. **T2** break-entry capture, then direct-RAM logging for the watched set.
   → ~30–40 Hz full-set.

Both end states are **without touching one byte of ECU flash**.

## 4a. Measured baseline + Tier 0 implementation (2026-11-11)

**Measured (real car, `captures/dde3_log.csv`, 5 ids incl. battery 0x0F65):**
45 rows over 17.6 s = **2.50 Hz**, deltas a rock-steady **398–400 ms** per
cycle. The wire log (`captures/dde3newlog.txt`) shows exactly one
`12 0F 0B 10 …` request + one `12 10 A0 …` reply per cycle — no BUSY,
no retries, no skips.

Cost model of that 400 ms cycle before the change:

| Item | Time |
|---|---|
| 55 ms hardcoded sleep between polls | 55 ms |
| TX 16 bytes @ 3 ms/byte pacing + per-byte drain | ~65 ms |
| ECU reply latency (unknown — the blind spot) | ? |
| RX 18 bytes @ 9600 (~19 ms) + 10 ms silence detection | ~29 ms |
| **known total** | **~149 ms** → leaves **~250 ms unexplained** |

The steady 399 ms (not jittery) says the missing ~250 ms is deterministic —
most likely ECU reply latency (its live job may run slower than the 0x2EF4
task scalars suggested, or the ECU's frame-end detection waits for line
silence after our TX). **This is now measurable, not guessable:**

Implemented (Tier 0, this commit):

1. **Timestamped wire log** — every `TX`/`RX`/`SIM`/… line in the protocol
   log is prefixed with `[+Δs]` = gap since the previous line. The next
car capture shows the TX→RX gap (ECU latency) vs. TX pacing vs. silence
   waits directly.
2. **55 ms poll sleep removed** (`src/app.rs` live loop) — next request goes
   out as soon as the reply is parsed. The transaction itself still costs
   ~50–100 ms, so no busy-spin.
3. **Live TX pacing 3 ms/byte → 1 ms/byte** (live path only; the flash-
   write paths keep their 3 ms default — writes stay as conservative as
   before). 1 ms ≈ the natural 9600 byte time (1.04 ms), so the frame is a
   continuous stream, no faster than the ECU's own transmissions.

**Expected:** ~85 ms/cycle saved → ~3.2 Hz if the ECU really takes ~250 ms
per reply; up to ~15–20 Hz if the missing time was tester-side after all.
**Next capture to read:** TX→`RX` gap on the 5-id poll. If ECU latency is
indeed ~250 ms, polling is ECU-capped and the only remaining lever is
Tier 2 (38.4 k break-channel RAM reads). A "listen, don't poll" mode is
dead: the 2026-11-11 passive capture proved the ECU transmits nothing
spontaneously (key-on and engine running; one short event at the key
turn, not saved) — the 16-byte block is simply the 5-id reply.

## 5. Known blind spots (honest list)

- **Boot block (`0x0000–0x7FFF`) missing from the dump** — `C3A9`'s boot-time
  value (and the DS2 interpreter's first jump targets) can't be confirmed
  statically. The T2 capture is the proof.
- **On-wire layout of the 4-byte break frames** (byte order, echo behaviour,
  `0x3E` PEC framing) is disasm-derived, capture-pending — `breakmode.rs`
  `BreakSim` models the mailbox behaviour, not the wire.
- **Flash framing contradiction** (handoff T3) remains untouched and is
  unrelated to logging; do not "fix" it from inference.
- The ECU's minimum re-request interval is inferred from the 0x2EF4 scalars
  (a few ms), not measured; the monitor capture in Tier 0 should time it.

## 6. References

| Thing | Where |
|---|---|
| Baud ladder, DS2 framing, break-protocol decode | `FINDINGS_baud_service_protocol.md` |
| Task list T1–T4, guardrails | `handoff-fast-baud.md` |
| Wire captures, HW topology, session history | `HANDOVER.md` |
| Live loop to patch | `src/app.rs:1234` (55 ms sleep), `src/protocol.rs` (`write_delay`, `set_silence`) |
| Break-mode implementation | `src/breakmode.rs` (`enter`, opcode map, `BreakSim`) |
| Disassembly | `~/Desktop/edc15m_out/research/out/disasm.txt` |
| Calibration (task periods) | `edc15m_out/variants/M7789659.bin` @ `0x3AEF4` (cal `0x2EF4`) |
