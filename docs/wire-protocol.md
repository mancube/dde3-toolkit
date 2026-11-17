# DDE3 wire protocol (on-the-wire facts)

Everything in this file is confirmed against real captures unless noted
otherwise. These notes supersede every earlier hypothesis about the framing
(in particular: there is **no** `B8 12 F1 LEN` envelope on DDE3 — that was a
DDE4 assumption and is wrong for DDE3).

## 17. Session-5 findings: wire framing CONFIRMED by hardware capture (2026-11-09)

### Method

Passive K-line sniffer (`EDC15C4-decomp/analysis_artifacts/tools/kline_sniff.py`,
9600 8E1, read-only) on `/dev/ttyUSB0` while WinKFP on the other adapter drove
the bus. The sniffer sees exactly what the ECU receives (tester TX only — no
ECU replies visible at this tap). Captures: `analysis_artifacts/tools/
sniff_session{,2,3,4}.raw`, `sniff_bg.raw`.

### What was on the wire

| Time (s) | Raw bytes | Job (from decompiled_winkfp.txt / frames.md) |
|---|---|---|
| 2.56 | `12 04 0d` + `1b` | UPROG_EIN `@ 0x02e7` |
| 7.27 | `12 04 00 16` | IDENT `@ 0xf3b4` |
| 20.80 / 27.77 | `12 04 0d 1b` | UPROG_EIN |
| 46.71 | `12 05 90 56` + `d1` | SCHALTE_PROTOKOLL_DS2_NACH_K2 `@ 0x1081d` |
| (sess. 3) | `1a 04 0d 1b` | UPROG_EIN with a 1-bit flip in byte 0 (`12`→`1a`); XOR no longer validates — checksum catches line errors |
| (sess. 2/4) | `12 04 0d 1b` | UPROG_EIN |

### Findings

1. **The `B8 12 F1 LEN payload XOR` hypothesis is WRONG.** No `0xB8` prefix,
   no tester address (`0xF1`), no length byte. The on-wire frame is
   **`<DS2 payload> <XOR of all payload bytes>`**. DDE3 does not use the
   KWP2000\* B8 envelope that DDE4 uses.
2. **XOR checksum confirmed**: `12^04^0d=1b`, `12^04^00=16`,
   `12^05^90^56=d1`. One bit flip breaks it — usable for resync/error
   detection.
3. **Timing**: WinKFP inserts a ~50 ms gap before the final XOR byte
   (e.g. `12 04 0d` at 2.557 s, `1b` at 2.607 s). Frames must be delimited by
   line silence > ~60 ms; a parser keyed on short silences will split frames.
4. **No ECU replies visible** at this tap — the sniffer only sees tester TX.
   Reply framing still needs a capture on the ECU side of the bus.
5. ECU internal temperature read: not supported (WinKFP fell back to the
   DS2→K2 protocol switch `12 05 90 56` instead).

### Code changes (this session)

`src/protocol.rs`:
- `build_frame(payload)` → `payload + xor_cs(payload)` (no B8/F1/LEN).
- `Frame` struct: dropped `target`/`source` (not present on the wire).
- `FrameParser`: now `feed()` accumulates, `take_frame()` validates the
  trailing XOR after a silence and resyncs by dropping leading bytes;
  incomplete frames (XOR byte still on its way) are kept across reads;
  buffer capped at 64 bytes.
- `Kline::request()` updated to the feed/take_frame flow.
- Removed `TESTER_ADDR`, `FMT` constants.

Tests: `cargo test` → 12 passing, including new `captured_winkfp_frames`
(byte-exact frames from the capture) and
`parser_keeps_partial_frame_across_reads` (the 50 ms gap case).

---


### ECU identity (captured 2026-11-09, `analysis/captures/ecu_identity.txt`)

- Reply to `12 04 00 16`: `92 04 A0 44 44 45 33 2E 30 20 46 55 45 52 20 4D 34 37`
  = **"DDE 3.0 fuer M47"** — the real ECU is an M47 engine DDE 3.0.
- The real ECU answered all services the same day (ident, DS2 fault read,
  seed/key `12 08 90 42 4D 57 05 00`, and a **full 256 KB flash write** that
  completed 11:21–11:24).


## 21. ⚠️ REAL-CAR FINDING (session 12) — ECU replies with `0x12`, NOT `0x92` ← **READ THIS FIRST**

The toolkit was developed against a `frames.md`/WinKFP assumption that the ECU
answers with the DS2 response prefix `0x92` (`ECU_ADDR | 0x80`). **A real-car
capture (`dde3 toolkit log.txt`, 2026-11-09) proved that wrong.** On this
ECU the reply is prefixed with the ECU's own address **`0x12`** — not `0x92`.
This single mismatch was why ident, fault read, and live/battery all failed to
parse against the real car.

**Real-wire reply convention (authoritative, from the capture):**

```text
<12> <len> <SB> [<data ...>]   + trailing XOR on the wire
```

- Prefix byte = `0x12` (ECU address), **not** `0x92`.
- Status (JOBRESULT) at offset **[2]**; data from offset **[3]** (fault/flash)
  or offset **[5]** (MW live).
- `len` (offset [1]) = total wire length **including** the trailing XOR byte.
- The trailing XOR still covers all payload bytes (unchanged).

**Other real-wire facts discovered from the same capture:**

1. **IDENT replies in the normal envelope** with the ident string as data:
   `TX 12 04 00 16` → `RX 12 2E A0 37 37 38 36 …` (2026-11-10 capture,
   `captures/dde3newlog.txt`) — status A0, ASCII string from [3], 42 chars,
   zero-padded: `7786887 5201 1240 4801 0281001445 G521 00…`
   (software version / ECU code / ? / week-year / Bosch HW number / G code).
   The RX log line shows the 45-byte payload (the parser strips the trailing
   XOR): LEN byte 0x2E = 46 = 3 header + 42 data + XOR on the wire.
   (The earlier session had misread a truncated bare run as the reply, and
   the 43-char variant had counted that trailing 0x30 XOR byte as a fourth
   zero — foreign tools reject a 47-byte IDENT reply.)
   `Kline::identify()` strips the envelope; `ident_valid()` checks the bare
   string (4–48 printable chars), `parse_ident()` splits it into fields.
2. **A ~16-byte live block `12 10 A0 10 00 00 00 00 00 03 DE 0B 26 02 4A`
   was observed** (DE/DF and 25/26 cycling between sightings). **Unverified
   whether it is spontaneous or merely the 5-id MW reply** — in
   `dde3newlog.txt` every 16-byte frame directly followed our own `0B 10`
   request (no SKIP lines, no unpaired RX), so the "ECU's own periodic
   broadcast" reading is a hypothesis, not a capture fact. It must be
   distinguishable from the small fault/battery replies (it is, by `len`),
   which is why the length/`is_not_live_broadcast` filters exist.

   **Settled 2026-11-11 by a real-car passive `capture` (ECU key-on and
   engine running): the ECU transmits NOTHING spontaneously** — no periodic
   live block; only one short event at the key turn (not saved). The DS2
   link is strictly request/response. Consequences: a "listen, don't poll"
   logger does not exist at 9600, the 16-byte block is the 5-id reply, and
   the only speed levers are tester-side pacing (Tier 0) and the 38.4 k break
   channel (Tier 2). If the key-turn event ever matters, re-run `capture`
   with `--out` while cycling the key.
3. **MW live reply** to `12 0F 0B 10 <5 ids>`: `12 10 A0 10 00 <u16 BE per id>
   ` — for ids `[0F83 0F10 0F40 0F00 0F65]` the values were `[0, 0, 990, 2854,
   586]` = PWG 0, N 0, boost 990 hPa, WTF 2854 (→12.3 °C), batt 586 (→11.9 V)
   (engine-off, ~12 °C — consistent).
4. **Battery (single-id) reply** is smaller: `12 1B A0 …` / `12 08 A0 10 00
   <u16>` (27-byte / 8-byte forms seen). Distinguished from the 16-byte block
   by its `len` field.

**Code changes (all in `src/protocol.rs` + `src/sim.rs`):**

- Reply prefix checks changed from `ECU_ADDR | 0x80` (`0x92`) to `ECU_ADDR`
  (`0x12`): `live_values`, `read_dtcs`, `clear_faults_ds2`,
  `check_flash_read_reply`. `ident_valid()` rewritten for the bare string.
- **`FrameParser`** rewritten from tail-anchored `take_frame` to a
  front-anchored 3-case extractor using `xor_end()` (shortest-valid-prefix
  XOR), `MAX_BUF=192` (was `MAX_FRAME=64`) so echo+reply fit, and a
  tail-half resync on overflow.
- **`do_request(payload, timeout, accept)`** + **`request_filtered`**: drain
  *all* frames per chunk, log each as `RX`/`SKIP`, and return only the first
  frame satisfying `accept`. `is_not_live_broadcast(f)` (skip `f[1]==0x10 &&
  f[3]==0x10`) is used for the small fault/battery replies; `live_values` uses
  a **length-matching** filter (`f[1] == 6 + 2*n`) so a 5-id poll (whose reply
  equals the broadcast) still resolves while a 1-id battery read skips the
  16-byte block.
- **`src/sim.rs`** updated to the `0x12` convention so `--sim` stays consistent
  (status/nack/ident/mw all emit `0x12`, not `0x92`).
- **5 new real-car fixture tests** (bytes taken verbatim from the capture):
  `real_car_live_reply_splits_echo`, `real_car_live_values_decode`,
  `real_car_ident_reply_is_raw_string`, `ident_valid_accepts_plain_string`,
  `is_not_live_broadcast_distinguishes`.

**Status:** `cargo build` clean, `cargo test` → **48 passed / 0 failed**
(43 pre-existing + 5 new). **Re-test against the real car is the next step** —
ident, fault read, and live/battery should now parse correctly end-to-end.

