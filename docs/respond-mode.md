# `respond` — fake-ECU responder on a single K-line port

**Session 2026-11-16/17.** This document covers everything done in the
session that added the `respond` subcommand and fixed the wire-format bugs
that made foreign flashing tools reject the simulated IDENT reply.

```text
dde3-toolkit respond [--port DEV] [--baud N] [--out FILE]
                     [--duration SECS] [--data-delay MS] [--reply-delay MS]
                     [--dump IMAGE.BIN] [--flash IMAGE.BIN]
                     [--patched-baud 38400|125000]
```

`--dump` writes the **reconstructed flash image** (everything the peer
programmed, contiguous first-to-last written byte) to the file at the end
of the run. The 192 KB test write (0x10000..0x40000) is exported as a full
256 KB image: the boot area 0x0..0x10000 is padded with 0xFF (erased
flash), so the dump lines up with a full flash read.

`--flash <image.bin>` loads a real 256 KB flash dump (0x40000 bytes) and
the fake ECU **serves it verbatim**: `FLASH_LESEN` (`12 09 06 …`) returns
the file's bytes at the requested address and the AIF read (`0x02`, slot
0x7CB2) returns the file's record — so an external tool reading the fake
ECU gets exactly what it would get from a real ECU holding that image
(e.g. `captures/actual_flash.bin`). Without `--flash` the built-in
deterministic pattern is served.

`--patched-baud <38400|125000>` configures the fake ECU like the **patched
real ECU** (the `e46_baud<rate>.bin` images, see
`docs/baud-0x91-patch.md`): a `0x91` baud-change frame is accepted and the
port switches to the patch's fixed rate (125000 → 124800, the S0BG divisor),
the ACK going out at the **new** rate — so `baud --rate …` and
`bench --fast-baud …` can be tested end-to-end without a car (see §9).

Single-port **full duplex** on one UART (default `/dev/ttyUSB0`, the
QinHeng UART-LPT): the adapter both *receives* the peer's frames (a
foreign flashing tool or another driver adapter wired to the same K node)
and *transmits* the simulated ECU replies. No second port, no dual
adapter — the K node is shared, the tool is the master, we are the slave.

## 1. What was built

### 1.1 `src/respond.rs` (new)

Full-duplex fake ECU. Reuses the existing pieces, no new state machine:

- **RX path** — same `capture::Framer` as the passive `capture` command
  (anchor `0x12`, **LEN-byte-first** frame termination with the
  shortest-XOR-prefix rule kept as resync fallback, verbatim RAW runs for
  silence-delimited garbage — see §6 for why LEN-first). Every extracted
  frame is logged as `RX` and fed to the simulator.
- **TX path** — `SimEcu` reply → `protocol::build_frame()` (payload +
  trailing XOR) → `Kline::write_frame`, optionally paced byte by byte
  (`--data-delay`, **default 0 in respond mode** — back-to-back, like the
  real ECU; the 3 ms/byte WinKFP pacing is a master-side habit).
  `--reply-delay` pauses between the peer's last byte and the first reply
  byte (default ~ a few ms) so the tool's RX window is not violated.
- **Echo suppression** — the adapter's RX sees its own transmissions.
  Each transmitted reply payload is compared against the next extracted
  frame; a match is logged as `ECHO` and *not* re-processed, so our own
  replies never re-enter the state machine.

Line format (same convention as `capture`; timestamp = seconds since
start, one line per event):

```text
[  12.345] RX   12 04 00 16   IDENT
[  12.400] TX   12 2E A0 37 37 38 36 …   IDENT-REPLY
[  12.500] ECHO 12 2E A0 37 37 38 36 …   (own reply, bus echo)
[  12.600] RAW  CF CF CF
```

`--out FILE` mirrors the log for `tail -f`; `--duration` stops after N
seconds (default: run until Ctrl-C). A summary (frames, replies, per-
service counts) is printed on exit.

### 1.2 Plumbing changes

- `capture.rs` — `Framer::new()` now takes an anchor parameter
  (`Framer::with_anchor()`); `STOP` and `install_sigint` made `pub` so
  `respond` can share the SIGINT plumbing.
- `protocol.rs` — `Kline::write_frame` made `pub` (respond writes wire
  frames directly).
- `main.rs` — `respond` wired in as a subcommand with the flags above.
  In `respond` mode only, `--data-delay` defaults to **0** when not
  explicitly given (the global default stays 3 ms for master-side
  commands).
- Default port for `capture`/`respond` is `/dev/ttyUSB0` (driver
  commands keep `/dev/ttyUSB1`).

## 2. The IDENT rejection — root cause and fix

The foreign tool sent `12 04 00 16` (IDENT) and **rejected** our reply.
Byte-level analysis of the real-car capture
(`captures/dde3newlog.txt`) vs. our reply found **two** bugs, both in
`SimEcu`:

### 2.1 The ident string is 42 chars, not 43

The RX log line for the real reply is

```text
RX 12 2E A0 37 37 38 36 38 38 37 35 32 … 30 30 30 30    (45 logged bytes)
```

`FrameParser::take_frame()` **strips the trailing XOR before logging**
(“returns the payload, trailing XOR byte stripped”), so the wire frame is
46 bytes and the 45th logged byte (`0x30`) is the last data byte, *not*
a checksum. The old simulator string was 43 chars — the original analysis
had counted the trailing XOR byte `0x30` (which looks like ASCII `'0'`)
as a fourth zero of the zero-padded string.

| | sim (before) | real ECU |
|---|---|---|
| payload | `12 2E A0` + **43** chars | `12 2E A0` + **42** chars |
| wire (after XOR) | **47** bytes | **46** bytes = LEN byte `0x2E` ✓ |

47 ≠ 46 → the tool drops the frame. Fix: the string in `SimEcu` is now
`b"77868875201124048010281001445G521000000000"` (42 chars).

### 2.2 LEN byte semantics: total wire length including trailing XOR

Cross-checking **every** ECU reply in the capture:

| Reply | Logged payload | LEN byte | Wire (payload + XOR) |
|---|---|---|---|
| IDENT | 45 | `0x2E` = 46 | 46 ✓ |
| STATUS (`12 05 A0 00`) | 4 | 5 | 5 ✓ |
| MW live | 7 | 8 | 8 ✓ |
| DTC count | 26 | `0x1B` = 27 | 27 ✓ |

**LEN = total frame length on the wire, trailing XOR included.**

But `SimEcu` was *echoing the request's LEN byte* in its replies, so
every service answered with the wrong LEN (IDENT request LEN 4 → reply
LEN 4 instead of 46; DTC request LEN 5 → reply LEN 5 instead of 27; …).
The toolkit's own `do_request` never validates LEN (it frames on the
XOR terminator), which is why `--sim` bench mode never noticed — but
foreign tools do validate it.

Fix: `SimEcu::handle()` now stamps every reply with
`r[1] = r.len() + 1` (payload + trailing XOR). The `0x0A`
(STATUS_CODIER_CHECKSUMME) data was also corrected to match the real car
byte-for-byte: `12 05 A0 00` (one `0x00` data byte, not a 2-byte
checksum — the old 2-byte variant was unused by anything, `read_app`'s
result is discarded).

## 3. Verification

- `cargo build` clean; **all 73 tests pass** (72 after §6, + the seed/key
  round-trip). Tests updated:
  - `sim::tests::ident_and_status` — asserts the 45-byte IDENT payload
    (42-char string, LEN `0x2E` = payload + XOR).
  - `sim::tests::flash_read_round_trip`, `fault_memory_sequence` — LEN
    assertions updated to `payload + 1`; the fault-count reply now
    produces `12 1B A0 …`, exactly the real-car DTC reply in the
    capture.
  - `protocol::tests::real_car_ident_reply_is_envelope_string`,
    `parse_ident_fields` — 42-char string, comment documents the LEN
    breakdown (3 header + 42 data + XOR = 46).
- **On-car:** the foreign flashing tool accepted the IDENT reply and
  proceeded (the job that previously aborted now continues).

## 4. What to test next on the fake ECU

The `RX` lines of a `respond` session are the tool's complete flow, so
any next rejection is visible in the log. Status as of the §6 follow-up:

1. ~~AIF / programming record~~ — **done**: `12 05 02 <param>` answers the
   46-byte record from cal `0x7CB2` (`12 32 A0 …`); empty slot → `12 05
   A0 FF`.
2. **DS2 / mode change** (`12 04 0D`) and back — works.
3. **Flash read** (`12 09 06 …`) — works (round-trips the sim image; with
   `--flash <file>` it round-trips the file's bytes, tested end-to-end with
   Galletto reading 0x80-byte blocks across the first ~208 KB). The AIF/
   hardware-ID read (`addr=0x000000`, `count=46`) serves the flash image's AIF
   slot content — the same 46 bytes the `12 05 02` programming-record handler
   serves (`12 32 A0 <46 B> + XOR`, LEN 50; empty slot → short `12 05 A0 FF`) —
   so with `respond --flash captures/actual_flash.bin` the real car's AIF
   record (Bosch HW number `0281001445`, ECU type fields, VIN data) is
   answered again instead of a hard-coded string; the `SimEcu::set_aif`
   override still wins for content-matched tool tests.
4. ~~Flash write~~ — **done** (see §6): 73-byte blocks parse intact, the
   image is reconstructed and `--dump`ed.
5. **Fault memory** (count / entry / shadow / tail) — LEN now matches
   the real capture; the fake ECU now serves a **single** stored fault —
   the real M47 reference error record (`analysis/actual error.txt` +
   `protocol::dtc_from_record_reference_m47`: ORT `0x08` Gluehanlage, ART
   mask `0xC8`, error frequency `60`, Errorcode `08 C8 3C …`) — and the
   DS2 clear (`12 04 05`) wipes it, so the next read answers `A0 <anz=0>`;
   the read reports **one** error, the clear leaves the memory empty.
6. **Flash end / restart** — note `0x17` is NACKed in normal mode by the
   sim; if the tool stalls after “write complete”, look here.

## 6. The 73-byte write truncation — LEN-first framer (2026-11-17 follow-up)

With IDENT/seed/key/AIF all accepted, the tool reached `FLASH_SCHREIBEN`
and wrote four 64-byte blocks (each a 73-byte wire frame `12 49 07 02
… <64 data> <XOR>`) without issue — then **write #5 (addr `0x10100`)
fragmented**: the Framer cut it at byte 21 (a 22-byte “frame” + 49 bytes
of RAW) and the tool retried the same block every 5 s forever.

Root cause: the Framer’s old termination rule was *pure* shortest-XOR-
prefix — the smallest `k` with `xor(buf[0..k]) == buf[k]`. For arbitrary
binary payloads some in-band byte coincidentally satisfies that equality
(write #5’s payload did, at index 21), so the frame was truncated
**data-dependently** — writes #1–4 happened not to collide. Any XOR-
prefix-only framer has this failure mode.

Fix (`capture.rs`): the LEN byte at index 1 is the authoritative total
wire length (including the trailing XOR — verified on every frame in
capture). The Framer now: (1) while fewer than LEN bytes are buffered,
**waits** (never cuts early, no in-band coincidence can truncate); (2)
when LEN bytes are present, validates the trailing XOR at index LEN−1
and emits the frame; (3) only if the declared LEN is implausible or the
declared terminator fails validation does it fall back to the old
shortest-XOR-prefix resync (kept for passive-capture robustness against
corruption / false anchors). `MAX_BUF` raised 192 → 512 to hold a full
73-byte frame plus resync slack. Regression test
`capture::tests::long_write_with_in_band_xor_collision_stays_intact`
replays a 73-byte frame with a forced collision at index 21 (all-at-once
and chunked feeds); both parse as exactly one 73-byte frame, zero RAW.

Also in this follow-up:

- **Seed/key implemented** (was a stub ACK): see `simulator.md` §Seed/key
  and `sim::tests::seed_key_round_trip`.
- **`--dump IMAGE.BIN`** — at run end, `respond` writes the reconstructed
  flash span (`SimEcu::written_span()` → `flash_image()`) to the given
  file, so the flasher’s image is recovered from a session. The 192 KB
  test write (0x10000..0x40000) is exported as a full 256 KB image with
  the boot area 0x0..0x10000 padded 0xFF (`dump_image`).
- **Clean SIGINT exit** — an interrupted blocking read (`EINTR`) now
  breaks to the summary/dump path instead of surfacing as
  `ERROR: Interrupted system call`.
- Zero-data `07 02` write blocks (hand-shake) are ACKed even outside
  programming mode; only real data writes require UPROG_EIN first.

Smoke-verified on a pty loopback: IDENT → UPROG_EIN → seed/key
(computed key accepted) → two 73-byte writes (one carrying the
in-band collision) → SIGINT; dump reproduced both 64-byte blocks
byte-for-byte (`0x10000..0x10080`, 128 B).

## 7. Files touched (cumulative)

| File | Change |
|---|---|
| `src/respond.rs` | **new** — full-duplex fake-ECU responder |
| `src/main.rs` | `respond` subcommand + flags; data-delay default 0 in respond mode |
| `src/sim.rs` | IDENT string 43→42 chars; LEN stamped as payload+XOR on every reply; `0x0A` data corrected to match real car; **AIF 46-byte record from cal 0x7CB2**; **seed/key implemented** (46-byte seed telegram, key verify, `written_span`/`flash_image` for reconstruction); zero-data `07 02` ACKed out of prog mode; tests updated |
| `src/capture.rs` | `Framer::with_anchor()`; `STOP` / `install_sigint` made `pub`; **LEN-first frame termination** (fixes the 73-byte write truncation) + `MAX_BUF` 192→512 |
| `src/protocol.rs` | `Kline::write_frame` made `pub`; IDENT tests/docs updated to 42 chars |
| `src/main.rs` | `--dump` flag for `respond` |
| `docs/wire-protocol.md`, `docs/simulator.md` | 42-char ident, LEN semantics, AIF 46-byte record, seed/key |
| `src/sim.rs`, `src/respond.rs`, `src/main.rs` (2026-11-17) | **`--flash <image.bin>`**: `SimEcu::with_flash_image()` / `from_flash_file()` serve a real 256 KB dump verbatim (FLASH_LESEN + AIF read); `--flash` flag + header line; `SEED_KEY_BIAS` removed — unbiased formula `K[i] = resp[0x12+i] + resp[Z+i] + resp[0x29+i]` verified against all three wire vectors (`Z=0x07/0x10/0x16`); tests `flash_read_serves_file_image`, `flash_image_wrong_size_rejected` |
| `src/sim.rs` (2026-11-17, MPPS follow-up) | **Missing DS2 responses implemented**: session keep `9E`, test stamp `0E`/`0F`, actuator override `0C`, EWS sync `6C`/`6D`, adjustment values `40`/`41`/`42`, MW sub-services `03`/`04`/`11`/`12`/`07`, flash-write-type `07 10` (STEUERN_TESTPLATZ) — all answered with well-formed JOBRESULT replies instead of falling through to NACK; tests `missing_ds2_responses_are_answered`, `mw_subservices_answered` |
| `src/sim.rs` (2026-11-19, MPPS follow-up) | **AIF/hardware-ID read content REVERTED**: `FLASH_LESEN addr=0 count=46` again serves the flash image's AIF slot (`self.flash[AIF_SLOT..AIF_SLOT+46]`, `set_aif` override wins) instead of the hard-coded ASCII `IDENT_STRING` from session 19 — the `respond --flash <file>` override therefore serves the real car's AIF record again (Bosch HW number / ECU type / VIN data), restoring the properly-read VIN; test hook `SimEcu::set_aif()` + tests `aif_override_serves_test_content`, `flash_read_aif_serves_slot_content` |
| `src/sim.rs` (2026-11-19, MPPS follow-up) | **Fault memory now single-error**: `SimEcu::default_faults()` returns one well-formed 50-byte fault record (the real M47 reference error: ORT `0x08` Gluehanlage, ART mask `0xC8`, freq `60`, Errorcode `08 C8 3C …`) instead of three placeholder records; `fault_memory_sequence` / `fault_replay_assembles_full_records` updated — the FS_LESEN sequence (count/entry/shadow/tail) walks the single error cleanly and the DS2 clear (`12 04 05`) empties the memory (next read → `A0 <anz=0>`) |
| `README.md` | `respond` documented |

See also [`wire-protocol.md`](wire-protocol.md) (framing, LEN semantics,
live block) and [`simulator.md`](simulator.md) (SimEcu services).

## 8. Missing DS2 responses implemented (2026-11-17, MPPS follow-up)

Testing the fake ECU against MPPS (`respond --dump …`) left the tool stalling
right after the DTC read/clear round-trip: the responses for several DS2
services the tool uses were still falling through to the `12 04 FF` NACK
(handler map `0x26900` in the decomp). All of them are now implemented in
`SimEcu::handle`:

- DS2 session keep `9E` / end `9F` — status only;
- DS2 test stamp read `0E` (status + 3 stamp bytes, LEN = payload + XOR = 7) /
  write `0F` (status, stores `b1 b2 b3`);
- DS2 actuator override `0C` — status only;
- DS2 EWS sync start `6C` / status `6D` — status only;
- DS2 adjustment value read `40` (status + u16 BE value, LEN = 6) / set `41`
  (RAM copy, status) / store `42` (EEPROM, status);
- DS2 MW sub-services `03` (measurement block: header + 25 u16), `04`
  (digital inputs: header + 4 bytes), `11` (PSG value read: `11 00` header +
  u16 BE), `12` (PSG select: status), `07` (STEUERN_TESTPLATZ: status, accepted
  if unlocked) — previously the generic sub-dispatch answered every unknown sub
  with the MW-select shape or NACK;
- DS2 flash-write-type `07 10` (STEUERN_TESTPLATZ) — status only, accepted if
  unlocked (was NACK via the flash sub-function chain).

Regression tests: `sim::tests::missing_ds2_responses_are_answered`,
`sim::tests::mw_subservices_answered`.

## 9. 0x91 baud patch — `--patched-baud` (session 34)

The fake ECU can now behave like the patched real ECU (see
[`baud-0x91-patch.md`](baud-0x91-patch.md)), which makes the toolkit's own
fast-baud path testable end-to-end without a car:

```sh
# Fake ECU on the tap port, patched for 125000
./target/release/dde3-toolkit respond --out fake_ecu.log \
    --patched-baud 125000

# Tester side on the other port: the full 0x91 flow
./target/release/dde3-toolkit baud --rate 125000 --port /dev/ttyUSB1
./target/release/dde3-toolkit bench --fast-baud 125000 --live
./target/release/dde3-toolkit bench --fast-baud 125000 --read --out img.bin
./target/release/dde3-toolkit bench --fast-baud 125000 --write --image img.bin
```

Wire semantics mirror the real patch exactly:

- The `0x91` arrives at the old rate; the fake ECU reopens its port at the
  patch's rate **before** the ACK is transmitted, so the ACK rides the line
  at the **new** rate (the patch writes S0BG before the ACK goes out — the
  tester must not expect a reply at the old rate).
- The frame's baud field is ignored — the patch's **fixed** rate is used
  (the shipped v1 patch images are single-rate), and 125000 runs at the
  S0BG divisor 124800, the same mapping the tester side uses to reopen.
- The link stays at the new rate until `respond` is restarted (= power
  cycle; the real ECU reverts to 9600 after sleep/wake).
- Without `--patched-baud` the legacy stub ACK stays (no switch), so
  `--sim` fast-baud runs keep working as before.

Implementation: `SimEcu::set_baud_patch(rate)` arms the sim; the `0x91`
handler records a pending switch (`take_baud_switch()`, consumed once) and
ACKs; `respond` reopens the port at the rate before sending the ACK and
tracks the link rate (`BAUD old -> new` log line, "link left at …" at
exit). Only the two shipped patch rates are accepted (38400 / 125000).

Verified end-to-end over a pty loopback (session 34): `baud --rate
125000` / `38400` complete the switch + IDENT verify; `bench --fast-baud
125000 --live` polls at ~89 Hz; a full 256 KB fast read matches the served
image byte-for-byte; a full fast write reconstructs (`--dump`) to the exact
source span (boot area 0x0..0x10000 padded FF, by design).

Tests: `sim::tests::baud_patch_91_switches_link_and_acks`,
`sim::tests::baud_91_without_patch_keeps_stub_ack`.
