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
   channel (Tier 2 — since removed: DS2 is the only protocol the ECU can
   actually use). If the key-turn event ever matters, re-run `capture`
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
- **`src/sim.rs`** updated to the `0x12` convention so the sim stays
  consistent (status/nack/ident/mw all emit `0x12`, not `0x92`).
- **5 new real-car fixture tests** (bytes taken verbatim from the capture):
  `real_car_live_reply_splits_echo`, `real_car_live_values_decode`,
  `real_car_ident_reply_is_raw_string`, `ident_valid_accepts_plain_string`,
  `is_not_live_broadcast_distinguishes`.

**Status:** `cargo build` clean, `cargo test` → **48 passed / 0 failed**
(43 pre-existing + 5 new). **Re-test against the real car is the next step** —
ident, fault read, and live/battery should now parse correctly end-to-end.


## 28. FrameParser: LEN-validated framing (2026-11-20) ← **current parser rules**

The front-anchored "shortest XOR-terminated prefix" extractor (section 21)
is **gone**. It truncated any reply whose data bytes happened to self-
terminate — the 46-byte SEED reply `12 2E A0 00 01 02 … 29 9D` contains the
in-band run `12 13 14 15` with `0x12^0x13^0x14 == 0x15`, so a chunked
delivery (46 bytes = ~49 ms at 9600 baud — always chunked) made the parser
emit the 3-byte frame `12 13 14` from inside the still-arriving reply. The
toolkit then computed the key from 3 garbage bytes and its key TX collided
with the real reply in flight.

**Current rules (`FrameParser::take_frame`, `src/protocol.rs`):**

- A frame is `12 <LEN> <job> <data …> <XOR>` where **LEN (offset [1]) is
  the total wire length INCLUDING the trailing XOR** (the convention from
  section 21, now load-bearing: every captured frame checks out — IDENT
  `12 2E …` = 46 wire, AIF `12 32 …` = 50, flash write `12 49 …` = 73).
- A frame is emitted **only when all LEN bytes are present AND the
  trailing XOR validates**. A partial frame is never emitted, no matter
  what its bytes look like — there is no anchor scan for `0x12` inside the
  buffer and no shortest-prefix rule.
- Leading non-`0x12` bytes are dropped one at a time (collision debris);
  an implausible LEN (outside 4..=255) or a failed trailing XOR drops one
  byte and resyncs.
- `do_request` transmits, then waits for the **echo of its own TX**
  (reflected by the monitor tap) by `echo_timeout` (5 ms/byte, min 250 ms);
  a missing echo means the request never reached the bus → resend (max 3),
  instead of waiting out the full reply timeout. BUSY resends are unchanged
  (gated by `retry_on_busy`, capped at 50).

## 32. 192-byte flash blocks: TRIED AND REJECTED on the real car (session 32/33, 2026-11-18)

The known-safe block sizes are the Galletto sizes: 64-byte
`FLASH_SCHREIBEN` writes (`WRITE_BLOCK`) and 128-byte `FLASH_LESEN` reads
(`READ_CHUNK`). The frame builder allows up to 246 data bytes per frame
(`assert!(data.len() <= 0xF6)` in `frame_flash_write`), so an experimental
mode was built to send **192-byte** blocks/chunks in both directions
(`WRITE_BLOCK_EXP` / `READ_CHUNK_EXP` / `Kline::set_experimental_blocks`),
gated by CLI `--experimental` + the `WRITE192` confirmation word (GUI: LINK
toggle + `WRITE192`).

**Real-car result: the ECU cannot sustain 192-byte write blocks.**
On-car `write --experimental` (2026-11-18): the first two 192-byte
`FLASH_SCHREIBEN` frames (`12 C9 07 02 01 00 00 C0 …` @0x10000,
`12 C9 07 02 01 00 C0 C0 …` @0x100C0) each drew a 4-byte write-OKAY
(`12 04 A0 …` — status 0xA0, *not* the 16-byte `12 10 A0 10 …` live
broadcast); the third (`… 01 01 80 C0` @0x10180) got **no reply and no
echo** on the line, and after 3 resends the link was dead
(`no echo of service 0x07 … line dead or ECU not listening`). Two
"accepted" blocks followed by a hung ECU is not a workable write path.
The frame format was wire-valid (LEN = 9 + 192 = 0xC9 fits one byte); the
ECU's flash-write path simply does not take a 192-byte block. The mode was
**removed** (session 33); 64 B writes / 128 B reads are the only verified
sizes. Don't re-attempt larger write blocks without a capture proving the
ECU's flasher accepts them.

## 33. MPPS-style calibration write: 224-byte blocks + `07 0F` + DTC prep (2026-11-22)

The calibration-area write (`write_calibration` / `bench --write-cal`) now
mirrors **MPPS's** calibration write, tested with 100% on-car success
(`captures/mpps capture/mppswrite.txt`; side-by-side in
`docs/FINDINGS_mpps_vs_galletto_calibration.md`). This is **distinct from
§32**: §32 rejected **192-byte program-area** blocks (the ECU hung at
0x10000); MPPS's **224-byte calibration-area** blocks (0x38000–0x40000) are
proven by the capture. The full write (`write_full`, program area) keeps the
64-byte Galletto size (`WRITE_BLOCK`).

> **Why a 233-byte frame is accepted at all:** each 224-byte block is one
> 233-byte wire frame (`LEN 0xE9`), which is larger than the **128-byte**
> application DS2 receiver limit (firmware-confirmed at `0x1EB34`). It works
> because these frames are sent **after `UPROG_EIN`**, when the boot-block
> flash programmer (a second, bigger receiver) owns the K-line. See
> **`docs/ds2-rx-buffer.md`** for the two-receiver split and the unconfirmable
> "256" (boot block is all `0xFF` in every dump).

**Sequence** (`Kline::write_calibration`):

1. `begin_flash_session(true)` — IDENT → UPROG_EIN → AIF read → **DTC
   read+clear** → SEED_KEY → key. The DTC prep is MPPS's pre-flash step, sent
   **once** (the `dtc_prep` flag is true only for the first session of a full
   write):
   - `12 05 04 00` — FS_LESEN entry (read the fault count)
   - `12 04 05` — FS_LOESCHEN (clear the fault memory)
2. `12 09 07 0F 03 FC E0 00` — FLASH_SCHREIBEN_ENDE (`07 0F`) at **0x03FCE0**,
   sent **once**, before the erase (MPPS sends it twice; one is sufficient).
3. `12 09 07 06 03 80 00 00` — FLASH_LOESCHEN (erase @0x038000).
4. 147 × FLASH_SCHREIBEN: **146×224-byte blocks + 1×64-byte trailing block**
   (`CAL_WRITE_BLOCK = 0xE0`), spanning 0x038000–0x40000.
5. `12 04 9F` — DIAGNOSE_ENDE (end-of-write signal; kept, harmless — MPPS
   omits it, Galletto sends it).

The same DTC read+clear (step 1) also runs **once** before reads
(`read_full` / `read_range` → `read_flash` → `begin_flash_session(true)`),
matching MPPS's read session (`captures/mpps capture/mppsread.txt`).

**Code** (`src/protocol.rs`): `CAL_WRITE_BLOCK` (0xE0),
`frame_flash_write_end(addr)`, `Kline::read_and_clear_dtcs()`, and
`begin_flash_session(dtc_prep: bool)`. The `07 0F` builder and the 224-byte
block math are unit-tested; `sim::tests::mpps_style_cal_write_preamble`
drives the full preamble through the fake ECU. End-to-end
(`bench --write-cal` vs `respond` on a socat pty pair) writes 147 blocks and
the reconstructed 0x38000–0x40000 span matches the source image
byte-for-byte. (That socat E2E carries **no monitor-tap echo**, so it never
exercised the parser's echo path — which is how the §34 buffer bug slipped
through. Re-verify with a tap, or rely on
`parser_holds_full_mpps_write_echo`.)

## 34. FrameParser `MAX_BUF` 192 → 512: the 233-byte MPPS write echo (2026-11-23)

**Bug.** The MPPS-style cal write (§33) sends each 224-byte block as one
**233-byte** wire frame (`12 E9 07 02 <addr3> E0 <224 data> <XOR>`, LEN
`0xE9`). The `FrameParser` resync bound was `MAX_BUF = 192`, sized back in §21
for the small Galletto frames (≤ 73 wire). On a line with a monitor tap, the
ECU echoes our own TX back, so the whole 233-byte write frame arrives at the
parser. `feed()` saw `buf.len() > 192` and drained the front, keeping only the
tail half — **destroying the frame's `12 E9` header**. `take_frame` then
re-anchored on a `0x12` byte *inside the data tail* (a phantom anchor), read a
LEN that never completes, and swallowed the ECU's real reply. `got_echo` never
became true → 3 resends → `no echo of service 0x07 … line dead or ECU not
listening`. It was **data-dependent** (only when the drained tail held a
`0x12`), which is why a real-car write froze at one specific block instead of
every block.

**Fix.** `MAX_BUF` 192 → **512** in `src/protocol.rs` (matching the
`Framer` in `src/capture.rs`, which was already 512). The largest real frame
(233 B) plus a back-to-back reply (≤ ~130 B) now fits with margin, so the
resync drain never fires on a legitimate frame. The drain heuristic (keep the
tail half) is unchanged — it is still the last resort for genuine persistent
garbage.

**Regression test.** `protocol::tests::parser_holds_full_mpps_write_echo`
feeds the full 233-byte write frame (with a `0x12` planted in the data tail)
as the echo, then the 4-byte reply, and asserts both are extracted intact. It
**fails** on `MAX_BUF=192` (the echo is not extracted) and **passes** on 512.

**E2E proof.** A faithful K-line T-topology tap (two raw ptys; the bridge
forwards each side's TX to the peer *and* taps it back to the sender's reader —
no pty `ECHO`, so no feedback loop) + `respond` + `bench --write-cal` on an
image with a `12 FC` trap in block 5's tail: **old code** fails with the exact
`no echo of service 0x07` error; **fixed code** writes all 147 blocks and the
dump matches byte-for-byte.

## 35. Real-car capture (2026-11-23): K-line sleep/wake + write pacing parity with MPPS

First full real-car calibration write with the MPPS-style flow
(`captures/dde3-20261123-122003.log`, analyzed by
`tools/analyze_realcar_write.py`). Two findings, both now fixed in code.

### 35.1 The ECU's K-line module sleeps after ~20 s of line silence — and data frames don't wake it

Timeline of the capture (relative seconds):

| t | event |
|---|---|
| 16.2 | connect (fresh port open) → IDENT OK (reply **516 ms**), battery live OK |
| 40.3 | write attempt 1: bootstrap IDENT → **no reply** (23.3 s after last traffic) |
| 53.1 | write attempt 2: bootstrap IDENT → **no reply** (12.8 s after attempt 1) |
| 63.3 | **reconnect** (port close+open) → IDENT OK (473 ms) |
| 77.8 | write attempt 3: bootstrap IDENT OK → full 147-frame write OK |

Facts that pin down the mechanism:

- The dropped IDENTs had a **TX echo** (no `no-echo retry` annotation in the
  log) — the frames reached the bus; the ECU simply did not answer within the
  (effective ~750 ms) window.
- Attempt 2 came 12.8 s after attempt 1 and was dropped too — attempt 1's
  data frame did **not** wake the module (a data-wake with first-frame loss
  would have made attempt 2 succeed).
- The port close+open (reconnect) restored the link, and the ECU then stayed
  responsive for 187 s, including 24.6 s and 24.3 s gaps after the write.

Conclusion: the ECU's K-line interface sleeps after ~20 s of silence; plain
data frames are ignored while it is asleep; a **line-level pulse (break)**
wakes it. Flasher tools (MPPS, Galletto, WinKFP) get that pulse for free
because they open the port fresh for every session — `connect` in this tool
does too, which is why connect/ident at startup always worked while the
write-attempt bootstrap (same port, 20+ s idle) failed. The "faster 9600"
GUI toggle was a red herring: it only paces the live loop, and the
successful write ran at the default 3 ms/byte.

**Fix** (`src/protocol.rs`):
- `Kline::wake_kline()` — a best-effort `set_break()` / `clear_break()` (10 ms)
  nudge, then a **real port close+open** (`reopen_port()`: drop the port →
  line-down pulse → fresh open → line-up pulse — the exact wake the user's
  manual reconnect produced), then 100 ms settle + drain + parser/pending
  reset. The drain read treats the port's 10 ms soft timeout as "no more
  data" (a plain `?` turns the empty line into a hard "Operation timed out"
  error — see 35.3). `set_break` alone is adapter-dependent and was not
  reliably waking the module, so the port reopen is the actual wake.
- `begin_flash_session()`: the bootstrap IDENT (now 1000 ms, see 35.2) is
  retried **once** after `wake_kline()` on failure, with a log line
  (`IDENT no reply (…) — K-line wake break, retrying`). This makes the flash
  bootstrap self-healing the way the user's manual reconnect did.
- `identify()` timeout 500 → **1000 ms** (real ECU answers ident in
  473–516 ms — 500 ms was at the edge even for a healthy link).

### 35.2 Write pacing: 0 ms/byte is wire-identical and matches MPPS

At 9600 baud (10 bits/byte) a byte occupies the line for **1.0417 ms**.
Host-side per-byte pacing below that is invisible to the ECU — the UART TX
buffer keeps the wire stream continuous either way. The old 3 ms/byte
WinKFP habit therefore only added **host** time: a 233-byte write frame
cost ~700 ms of `write_all`+`flush`+`sleep` per byte.

Measured cadence for the 233-byte `FLASH_SCHREIBEN` frames:

| | per frame | 147 frames (32 KB cal) |
|---|---|---|
| our run, 3 ms/byte (this capture) | 830 ms | 130 s |
| MPPS reference (`captures/mpps capture/mppswrite.txt`) | 327 ms | ~48 s |

MPPS sends back-to-back; its 327 ms ≈ 242 ms of wire (233 × 1.04 ms) + the
ECU's reply. **Fix** (`src/app.rs`, `src/protocol.rs`):
- `start_write` / `start_write_cal` now pass `data_delay_ms = 0` (the
  `fast write` checkbox, which only selected this, is removed — it is now
  always on).
- `Kline::write_frame` fast path: with zero delay the frame goes out as one
  bulk `write_all` instead of per-byte `write_all`+`flush` (same wire
  bitstream, one syscall instead of 233).
- Expected real-car cadence: ~330–350 ms/frame (wire-bound + ECU reply),
  i.e. MPPS parity; 32 KB in ~50 s instead of ~130 s. **On-car re-verification
  pending** (the capture above ran the old 3 ms/byte path).

Reply latencies measured on the real ECU (for timeout sizing): IDENT
473–516 ms, UPROG_EIN 687 ms, AIF 543 ms, FS_LESEN 256 ms, FS_LOESCHEN
272 ms, seed 448 ms, key 256 ms, erase 623 ms, write-end 80 ms,
DIAGNOSE_ENDE 64 ms, SCHREIBEN reply ~50–100 ms after the last TX byte.

### 35.3 The v0.5 wake fix was itself broken — the retry never ran

> **Superseded** — the `wake_kline` / `reopen_port` / `baud`-field /
> `Option`-port machinery described here (and in §35.4) was later **removed
> entirely** once §35.5 showed pacing was the real fix. See §35.6. Kept for
> the record.

The 35.1 fix shipped in v0.5 (commit `176774e`) but the calibration write
still failed on the car (`captures/dde3-20261123-200017.log`,
`…-200207.log`). The logs show the tell-tale sequence:

```
TX 12 04 00 16
IDENT no reply (no response to service 0x00 (12 04 00), link /dev/ttyUSB0) — K-line wake break, retrying
ERROR calibration write failed: Operation timed out
```

Two things were wrong:

1. **The retry IDENT never ran.** `wake_kline()` drained the RX buffer with
   `while self.port.read(&mut buf)? > 0 {}`. The port has a **10 ms read
   timeout** (`Kline::open`); after `clear(ClearBuffer::All)` the line is
   empty, so that `read` blocks 10 ms and returns `Err(TimedOut)` =
   "Operation timed out", and the `?` propagated it as a **hard error**.
   `begin_flash_session`'s `self.wake_kline()?` therefore aborted *before*
   the retry IDENT was sent. The "Operation timed out" in the log is the
   port's `TimedOut` — **not** the custom "FLASH_READ bootstrap: IDENT failed
   (even after wake break)" context — which is the proof the retry never ran
   (there is no second `TX 12 04 00 16` in the capture).
2. **`set_break` is not the proven wake.** The manual reconnect that worked in
   v0.4.1 is a full port **close+open** (line-down → line-up pulse).
   `set_break()`/`clear_break()` is adapter-dependent; the adapter accepted
   the command (no "set_break unsupported" log) but that does not guarantee the
   ECU detected a break. Relying on it alone was the wrong bet for a real-car
   flash.

**Fix** (`src/protocol.rs`, this session):
- The `wake_kline` drain read now matches `ErrorKind::TimedOut` and treats it
  as "no more data" (break out of the drain) instead of propagating it. The
  retry IDENT in `begin_flash_session` now actually runs.
- `Kline::port` is now `Option<Box<dyn SerialPort>>` and a new
  `Kline::reopen_port()` closes the port (all fds → line-down) and reopens it
  fresh at the current baud (line-up) — the proven reconnect wake. `wake_kline`
  calls it after the best-effort `set_break`. A `baud` field is kept so the
  reopen uses the current rate; `set_baud` / `recover_link_for` update it.
- Regression test `protocol::tests::wake_kline_succeeds_and_port_stays_usable`
  opens a `Kline` on a libc pty and asserts `wake_kline()` returns `Ok` on an
  empty line (the old code failed it with exactly `Operation timed out`).

### 35.4 The wake ran too late — and `set_break` is gone

> **Superseded** — `wake_kline` no longer exists; see §35.6.

Even with the 35.3 fix (the retry IDENT now runs), the car logs
(`captures/dde3-20261123-200017.log`) show the bootstrap IDENT failing
*before* the wake could help: `begin_flash_session` sent the IDENT first and
only called `wake_kline()` **after** it timed out. The tell is that the IDENT
fails even **~2 s after a fresh connect** (20:01:30 connect, 20:01:33 write
IDENT no reply) — far too soon for a 20 s sleep, so the module re-sleeps
*between* transactions and only a fresh line pulse wakes it. Sending the
IDENT on a stale line and waking after the fact can never succeed on the first
try.

**Fix** (`src/protocol.rs`, this session):
- `begin_flash_session()` now calls `wake_kline()` **before** the bootstrap
  IDENT (not after a failure), so the IDENT goes out on a freshly-woken line
  — the same fresh-open wake the connect gets for free from `Kline::open`.
- `wake_kline()` no longer calls `set_break()`/`clear_break()`: it is
  adapter-dependent, was never the proven wake, and can leave the line
  dominant if `clear_break` is unsupported (which would itself block the
  reply). The port **reopen** (close → line-down, open → line-up) *is* the
  wake; the `set_break` nudge is dropped. The 35.3 drain-read timeout handling
  is unchanged and still required (the drain runs after every reopen).

### 35.5 The real root cause: the bootstrap IDENT went out unpaced (no sleep)

The 35.1 "sleep" theory is **not supported by the firmware decompilation** and
the idle gaps in the logs (16 s, 2 s, 13 s) are not consistent with a single
sleep timeout. The actual regression is a **serial-timing change** introduced
by the MPPS-speed pacing commit (`176774e`):

- `start_write_cal` / `start_write` now call `set_write_delay(0)` for the
  **entire** write, and `write_calibration` / `write_full` set it again at the
  top. So the **bootstrap IDENT** (and UPROG_EIN, AIF, seed/key, DTC prep,
  write-end, erase, DIAGNOSE_ENDE) all went out **unpaced (bulk `write_all`)**
  for the first time. In the working v0.4.1 build the IDENT went out at
  **3 ms/byte** and answered in 473–516 ms.
- The MPPS reference capture
  (`captures/mpps capture/mppswrite.txt`) confirms the intended split: the
  **data frames** (`FLASH_SCHREIBEN`, 236 B) run **~327 ms/frame back-to-back**
  (245 ms wire + ~80 ms reply — a 3 ms/byte pace would be ~710 ms/frame, so
  they are definitively unpaced), while the **bootstrap commands** are small
  frames sent at the normal cadence. MPPS finishes a 32 KB cal in ~50 s
  precisely because only the data frames are unpaced.

**Fix** (`src/protocol.rs`, this session): `write_calibration` / `write_full`
now set `write_delay` per phase — **3 ms/byte** for the bootstrap and
session-control commands (IDENT, UPROG_EIN, AIF, seed/key, DTC prep,
write-end, erase, DIAGNOSE_ENDE), and **`data_delay_ms` (0)** only around the
`write_block` data-frame loop. This matches MPPS exactly and is the actual fix.

**On-car re-verification pending:** one more `write-cal` — the bootstrap IDENT
should now succeed (paced, as in v0.4.1), and the data frames should run at
~327 ms/frame (MPPS parity).

### 35.6 The wake mechanism was dropped entirely

With §35.5 identifying pacing as the real fix, the port-reopen wake was
removed in full — it was a heavy close+open on every write, for a problem (the
"sleep") that the firmware decompilation does not support. Removed:

- `Kline::wake_kline()` and `Kline::reopen_port()` — gone.
- `Kline::port` reverted to `Box<dyn SerialPort>` (no longer `Option`); the
  `baud` field and the `serial()` accessor are gone.
- `begin_flash_session()` sends the bootstrap IDENT directly (no pre-IDENT
  wake call).
- The `protocol::tests::wake_kline_succeeds_and_port_stays_usable` pty
  regression test is gone.

The connect still gets a fresh-open wake for free from `Kline::open` (the
port is opened once at connect), and the paced bootstrap IDENT is the same
frame v0.4.1 sent successfully. If a paced IDENT ever times out on the car,
the line-state question comes back — but the v0.4.1→v0.5 delta isolates
pacing as the only thing that changed for the IDENT, so that is the fix to
verify first.

### 35.7 Firmware DS2 UART timing (state machine `0x1E904`, M7789659)

The C166 S0 UART is driven by a byte-wise state machine at `0x1E904` (state in
`C749`, 7 states). The UART RX **ISR** is a 3-byte stub at `0x1E900`
(`BSET S0CON.15; RET`) that only sets a byte-available flag; the state machine
is ticked by the main loop, which lives in the `0x0000` region (absent from
the app disasm, which starts at `0x10000`). This is the firmware-side basis
for the pacing decisions in §35.5–35.6.

**Timing constants** (`DS2_UART_TIMING`, `SCALARS@2EF4`; units = state-machine
ticks, `C6C0` += 2 per invocation — the wall-clock tick period is in the
`0x0000` region and is **not** recoverable from the app disasm):

| Offset | Value | Meaning |
|---|---|---|
| `+0` | 512 | min inter-byte gap while receiving (a longer gap *inside* a frame aborts it) |
| `+2` | 5120 | frame-end timeout (no byte this long → finalize the frame) |
| `+4` | 1024 | gap between the ECU's own response bytes (state 2, transmit) |
| `+6` | 6400 | delay before the ECU starts its response |
| `+8` | 62465 | dispatcher prepare timeout (state 0, idle) |
| `+A` | 10240 | 10.4 kbaud state-4 gap |

**Practical implications (all consistent with the dde3-toolkit's design):**

- **Frame completion is counter-based, not timeout-based.** The ECU finalizes a
  frame the instant the byte counter reaches the `LEN` byte; the `+2` (5120)
  timeout is only a fallback for *incomplete* frames. A well-formed 233-byte
  MPPS frame completes as soon as its last byte lands — no line silence needed.
- **The ECU paces its own responses.** State 2 inserts a `+4` (1024-tick) delay
  between each response byte and a `+6` (6400-tick) delay before the first
  byte. The ~327 ms/frame round-trip in the MPPS capture is dominated by the
  flash write plus this paced response, **not** by the host. The host cannot go
  faster than the ECU — the dde3-toolkit's unpaced data frames are already at
  the ceiling (see §33 / `ds2-rx-buffer.md`).
- **The `+0` (512-tick) min inter-byte gap is why the 3 ms/byte bootstrap
  pacing is safe.** At 9600 8E1 (104 µs/byte) a 3 ms/byte pace is ~29 ticks
  per byte, far under the 512-tick abort threshold. The exact tick period is
  unconfirmed, but v0.4.1 paced the IDENT at 3 ms/byte with 100% on-car
  success, so the margin is proven.
- **Break → baud switch (state 3).** A break condition waits for
  `C6C0==0x4C` then switches `S0BG` to ~10.4 kbaud. This is the
  *programming-entry* sequence; the dde3-toolkit (and MPPS in the capture)
  stay at 9600 and never trigger it.

**Not a speed constraint:** the 512/5120/1024/6400 values are *ECU-internal*
timeouts and paces. They do not cap how fast the host may send — the host's
only real ceiling is the ECU's flash-write time (the ~327 ms/frame).

## 36. Full read starts at 0x8000 — the ECU does not expose its IROM (session 48, 2026-11-26)

The first 0x8000 bytes of a DDE3 full read are **always 0xFF**: the ECU does
not expose its IROM over `FLASH_LESEN`. Real-car dumps confirm it
(`captures/actual_flash.bin`: `[0, 0x8000)` is all FF; the program area at
0x10000 is the first real content, and 0x8000..0x10000 is the boot/gap area
the ECU does expose).

So `Kline::read_full` no longer reads from 0x0:

- the wire read runs `[FLASH_READ_START = 0x8000, 0x40000)` — **1792 × 128 B
  chunks instead of 2048** (256 chunks / 12.5 % of the read time saved);
- the returned image is still the full 256 KB with `[0, 0x8000)` FF-padded,
  so the image layout, file output and all downstream consumers (checksum,
  write read-back compare over `[0x10000, 0x40000)`, GUI/CLI) are unchanged;
- the flasher session bootstrap (IDENT → UPROG_EIN → AIF read → SEED_KEY) is
  unchanged and still runs first — the AIF read (`0x02` slot 0x7CB2) is a
  separate service, not a `FLASH_LESEN`, and still works.

`read_range` (calibration read 0x38000..0x40000) is untouched. Constant:
`FLASH_READ_START` in `src/protocol.rs`; test:
`protocol::tests::full_read_skips_unexposed_irom_prefix`.
