# Flash read/write framing + the fast-DS2 (38.4 k / 125 k) path — resolved

Date: 2026-11-12. Grounds: `research/out/disasm.txt` (DS2 dispatcher + handlers),
`logger/decompiled_winkfp.txt` (WinKFP tester job templates), the real-car
capture `captures/dde3newlog.txt`, and the verified baud patch in
`../../patches/`. This supersedes the "flash framing contradiction (T3)" and
"Tier 3 rejected" notes with evidence.

---

## 1. The DS2 service dispatcher (verified from disasm 0x2932C–0x293D0)

The service byte sits in `R4`. The dispatcher compares it and jumps to a
handler; every handler ends at the common reply tail 0x295F6 → 0x29090.
Replies are prefixed with the ECU address **0x12**, status at [2] (0xA0 = OKAY,
0xA2 = refused, 0xFF = unsupported), `len` at [1] = total wire bytes incl. XOR.

| svc | meaning | handler |
|---|---|---|
| 0x00 | IDENT | 0x2695C |
| 0x02 | (info) | 0x26AFE |
| 0x04 | fault / shadow read | 0x26C0C (sub0) / 0x26CBA (sub1) |
| 0x05 | fault-related | 0x26D2C |
| **0x06** | **flash/area read** | **0x26DA2 — NACK stub in app mode** |
| **0x07** | **flash write/erase/end** | **0x26DBC — NACK stub in app mode** |
| 0x0A | checksum status | 0x26DD6 |
| 0x0B | live MW data | 0x29440 |
| 0x0C | actuator control | 0x294B8 |
| 0x0D | UPROG / programming-mode + test-code area | 0x2954C |
| 0x0E/0x0F | test code | 0x29556 / 0x29560 |
| 0x14 | fault read (chunked) | 0x2956A |
| 0x40/0x41/0x42 | adjustment read/set/store | 0x29588 / 0x29592 / 0x2959C |
| 0x53 | (test-bench) | 0x295A6 |
| 0x6C/0x6D | EWS sync / status | 0x295AC / 0x295CA |
| 0x90 | seed-key / K2 switch | 0x28B5E |
| 0x9E/0x9F | session keep / end | 0x2900A / 0x295E8 |
| else | unsupported → reply `12 04 FF` | 0x295F2 |

## 2. Flash read/write only works in PROGRAMMING MODE — this resolves T3

In the **normal application session** services 0x06 and 0x07 are stubs:

```
026DA2 (0x06):  CMP R0,FD86h ; stack-depth guard (reset 0x25 on overflow)
                MOVB CBFAh,#A2h   ; reply status 0xA2 (refused), nothing read
                RETS
026DBC (0x07):  identical shape — status 0xA2, no write.
```

So a bare `12 09 06 …` in a normal session returns a refusal, not flash data.
Real 256 KB read/write happens only after **programming mode** is entered with
`UPROG_EIN` (`12 04 0D`). The real flash programmer dispatcher lives in the
**boot block (file 0x0000–0x7FFF), which is not in any dump** — exactly the
EDC15C4 situation. Its on-wire frames therefore come from the WinKFP job
templates, not from static code.

**Toolkit read path (2026-11-20):** the bare `FLASH_LESEN` chunk loop
(`Kline::read_flash`) is now preceded by the full flasher session bootstrap —
IDENT (`12 04 00`) → UPROG_EIN (`12 04 0D`) → AIF read (`12 05 02 00`) →
SEED_KEY (seed `12 08 90 42 4D 57 Z` + key `12 08 90 K0 K1 K2 K3`) — before
the first `FLASH_LESEN` chunk. Previously the read sent `FLASH_LESEN` frames
directly; that did not match how Galletto read the full flash
(`captures/galletto capture with respond tool/testread.txt`: IDENT, UPROG_EIN,
AIF, SEED_KEY, then 2048 × `FLASH_LESEN` @128 B).

**Toolkit write path (2026-11-20, session 27):** the same full session bootstrap
now precedes the write sequence — `Kline::write_full` / `Kline::write_calibration`
run `begin_flash_session()` (IDENT → UPROG_EIN → AIF read → SEED_KEY) before the
erase → write → write-end, replacing the bare `UPROG_EIN` they used to send.
This mirrors Galletto’s write session (`fakewritegalleto.txt`: UPROG_EIN, IDENT
cycles, AIF, SEED_KEY, then FLASH_LOESCHEN → FLASH_SCHREIBEN →
FLASH_SCHREIBEN_ENDE).

### WinKFP tester frames (ground truth, `decompiled_winkfp.txt`)

All are issued **after** `UPROG_EIN` (`12 04 0D`). The job first sends an
`xsetpar` line-timing descriptor (e.g. erase uses `06 00 80 25 … 64 00 14 00
03 00`) — that is tester-side only, not an ECU frame.

| job | template | fields |
|---|---|---|
| FLASH_LESEN (read) | `12 09 06 00 00 00 00 02` | svc 0x06, [3]=00, **addr @ [4..6]**, **count @ [6..7]** |
| FLASH_LOESCHEN (erase) | `12 09 07 06 00 00 00 00` | svc 0x07, **subfn 0x06**, addr @ [4..6] |
| FLASH_SCHREIBEN (write) | `12 09 07 02 <addr:3> <data…>` | svc 0x07, **subfn 0x02**, addr, then data (LEN recomputed) |
| FLASH_SCHREIBEN_ENDE | `12 09 07 0F 00 00 00 00` | svc 0x07, **subfn 0x0F**, addr |
| BLOCKLAENGE_MAX | `12 09 06 00 00 00 00 01` | returns 0xFFF7 = max write-block length |

### RESOLVED: count = 1 byte, address big-endian (code now fixed)

Settled by **running the WinKFP job VM** (`tools/prg_vm.py`) with real params,
cross-checked against the job bytecode and reconciled adversarially (2026-11-12).
The decisive step was an over-range probe: FLASH_LESEN with count `0x1234` emits
`12 09 06 00 01 23 45 34` — only the low byte `0x34` reaches the wire, the frame
stays 8 bytes, LEN stays `0x09`. Therefore:

- **count is ONE byte** at offset 7;
- the **address is 24-bit big-endian** at offsets 4,5,6 (offset 4 = MSB), proven
  by the job's mask/shift constants (`AND 0xFF0000 >>16` to [4], `AND 0xFF00 >>8`
  to [5], `AND 0xFF` to [6]);
- offset 3 is a **constant 0x00** for read (service 0x06) and the **subfunction**
  for the write family (0x02 write / 0x06 erase / 0x0F end).

The old `frame_flash_read` was little-endian, dropped the offset-3 `00` byte
(7 payload bytes), yet kept `LEN=0x09`. A conformant DS2 receiver completes a
frame only when received-count == LEN, so it waited forever for a 9th byte and
never replied — **exactly what the real car did**: `captures/dde3newlog.txt`
shows `12 09 06 00 80 03 80 …` sent twice with no response. The "full 256 KB
write once worked" was a **simulator artifact** — the old sim decoded the same
wrong layout, so the round-trip was circular.

**Fixed in code this session:** `frame_flash_read` (BE + offset-3 `00`),
`frame_flash_write` (BE + 1-byte data-length at [7] + `LEN = 9+N`),
`frame_flash_erase` (BE, no count), `frame_flash_write_end` (BE), and the
matching `sim.rs` decoders (`addr_be` at [4..6], count/datalen at [7]).
`read_range`'s 0x80 chunk is a valid 1-byte count and is unchanged. Unit tests
assert the VM-extracted byte vectors.

**Still worth one bench capture** (confirmation, not a blocker): a real
`UPROG_EIN` + FLASH_LESEN round-trip of the *corrected* frame, to confirm the
reply data offset and the programming receiver's max write block (the datalen
field is 1 byte → ≤255 per frame; BLOCKLAENGE_MAX's 0xFFF7 is the max *aggregate*
length, not a per-frame cap). The write path stays gated behind typing WRITE +
a verified backup.

## 2.1 Full-write region — only `[0x10000, 0x40000)`, in **two sessions** (Galletto)

The Galletto full-write session (`captures/galletto capture with respond
tool/testwrite.txt`, and `fakewritegalleto.txt`) writes the program area and
the calibration area as **two separate, re-authenticated sessions**:

Phase 1 — program area `[0x10000, 0x38000)`:
- IDENT / UPROG_EIN (retried), AIF read, SEED_KEY (seed + key)
- erase `12 09 07 06 01 00 00 00` @ 0x10000
- 2560 × 64-byte `FLASH_SCHREIBEN`, 0x10000..0x37FC0
- `DIAGNOSE_ENDE` (`12 04 9F`) closes the session

Phase 2 — calibration area `[0x38000, 0x40000)`:
- session re-opened and **re-authenticated from scratch**: IDENT / UPROG_EIN
  (retried), AIF read, SEED_KEY (fresh seed + key)
- erase `12 09 07 06 03 80 00 00` @ 0x38000
- 512 × 64-byte `FLASH_SCHREIBEN`, 0x38000..0x3FFC0
- `DIAGNOSE_ENDE` closes the session

There is **no `FLASH_SCHREIBEN_ENDE` (`07 0F`)** anywhere — each phase ends
with `DIAGNOSE_ENDE`. Total: 3072 × 64-byte writes (2560 + 512).
`Kline::write_full` therefore runs the two phases above (bootstrap →
per-area erase → 64-byte blocks → DIAGNOSE_ENDE, then a full re-bootstrap for
the calibration area); `Kline::write_calibration` is phase 2 on its own.
The full read (`FLASH_LESEN` @0x0, 128-byte contiguous blocks, 2048 frames)
and the calibration read were verified to already match Galletto and were
left alone. The CLI `--write` sim read-back compare is scoped to the written
region `[FLASH_WRITE_START, IMAGE_SIZE)`.

Verified end-to-end against the fake ECU (pty loopback, `bench --write` vs
`respond --flash`): the respond log shows exactly the two-phase structure
(2× UPROG_EIN/SEED_KEY/LOESCHEN/DIAGNOSE_ENDE, 3072 writes, no `07 0F`) and
the reconstructed dump is byte-identical to the source image over
`0x10000..0x40000`.

### The `07 0F` write-end is deliberately NOT sent (decompilation gap, 2026-11-18)

The user asked for each zone (program area, calibration area) to end with
`FLASH_SCHREIBEN_ENDE` before `DIAGNOSE_ENDE` — "an addition like a
failsafe" — under the rule **"don't implement a command if it's not fully
understood from the job decompilation"**. Checked against
`before/10DDE30.prg`, job `FLASH_SCHREIBEN_ENDE` @0xD782
(`analysis/winkfp_proto.txt`, run with `tools/prg_vm.py`):

- **Understood:** frame skeleton `12 09 07 0F <A_hi A_mid A_lo> 00` + frame
  XOR (LEN `0x09`, svc `0x07`, subfn `0x0F`); reply = JOBRESULT status byte
  at [2] (`A0` OKAY / `A1` BUSY-retry / else error) — same as the rest of
  the `0x07` family. The zero-param VM run emits exactly
  `12 09 07 0F 00 00 00 00` (matches `analysis/frames.md`).
- **NOT determined by the decompilation:**
  - the **address value** at [4..6]: the job copies those bytes from the
    caller-supplied BIN-buffer string parameter (second `pary`); the .prg
    never computes an address itself. Parameterized VM runs show a
    buffer→frame mapping that is *not* the clean BE-address layout the read
    job proves with its mask/shift constants, and no real PC-software wire
    capture shows the value the actual job sends (Galletto never sends
    `07 0F` at all; the only `07 0F` on the wire in our captures is this
    toolkit's own pre-session-30 write, which used `0x10000` by guess and
    was only ever answered by the fake ECU).
  - **when / how often**: the .prg holds 33 leaf job functions and no
    orchestrator — no job jumps to 0xD782. The call order lives in the PC
    software's C code, which we don't have.
- **Decision:** keep Galletto's flow — each zone ends with `DIAGNOSE_ENDE`
  only. An uncommitted attempt to add `write_end()` per zone (working tree
  on top of `d584a92`) was reverted (kept in `git stash` for reference).
  Re-evaluate only when a real MPPS/PC-software capture shows the address
  and the timing.

## 3. Real-car capture confirms the rest (`captures/dde3newlog.txt`)

```
TX 12 04 00 16                         RX 12 2E A0 "7786887 5201 1240 4801 0281001445 G521 00…"   (IDENT)
TX 12 04 0A 1C                         RX 12 05 A0 00            (checksum status — previously "never captured")
TX 12 0F 0B 10 0F83 0F10 0F40 0F00 0F65 BF
                                       RX 12 10 A0 10 00 <5×u16 BE>   (batched live, LEN=5+2n confirmed)
TX 12 05 14 01 02                      RX 12 1B A0 …             (fault read)
```

So: the **0x12 reply prefix, the batched `LEN = 5 + 2n` live frame, and the
checksum-status reply `12 05 A0 00`** are all confirmed on the real car. The
earlier open items for those are closed.

## 4. The baud ladder and the fast-DS2 patch

The K-line is the C166 **S0** UART; baud = `9600 · 52 / (S0BG+1)` (anchored on
the stock init `S0BG=0x33` → 9600 at 0x1E8F4). Only three values exist in stock
firmware: `0x33`=9600, `0x2F`≈10.4 k, `0x0C`=38.4 k. DS2 is locked at 9600
because the receive state machine (0x1E904, states 0–2) never touches S0BG and
no stock service re-rates it.

Clean fast rates reachable by writing S0BG:

| rate | S0BG | actual | error |
|---|---|---|---|
| 38400 | 0x0C | 38400 | 0 % |
| **125000** | **0x03** | **124800** | **−0.16 %** |

125000 is the **N=3 divisor** that other C166/C167 BMW fast-loggers use; it is
within UART tolerance. 57600/115200 are **not** cleanly reachable (non-integer
divisor) and should not be attempted.

### The patch (../../patches/patch_baud.py) — adds DS2 service 0x91

It hooks the dispatcher fall-through (0x293D0) to a 32-byte trampoline+handler
in the erased block at 0x185C3. On `12 08 91 00 <baud BE> 03` it ACKs and writes
`S0BG` to the selected divisor. The DS2 protocol is otherwise unchanged, so the
ECU answers DS2 at the new rate and **all existing toolkit DS2 code works once
the port is reopened**. Revert = power cycle.

Two corrections vs. the older `baud91_patch/` draft (which would have bricked /
not switched):

1. **Checksum:** the hook + trampoline lie in the cold-start **program-chain**
   checksum region (0x10000–0x38B7F), not just the two TDI slots. The balancing
   word at 0x38B7C must be recomputed. The builder does this
   (`edc15m_patch.fix_checksums`); the old note's "no checksum impact" was wrong
   and would have caused a reset-loop (reset code 9).
2. **Jump encoding:** the dispatcher (segment 2) → trampoline (segment 1) jump
   crosses segments and must be **JMPS** (`fa 01 c3 85`), not **JMPA**
   (`ea 00 c3 18`, which lands at 0x0218C3 and crashes). Likewise the
   trampoline's forwards to 0x295F2 / 0x295F6 are JMPS. All encodings were
   verified with the project disassembler.

Prebuilt, checksum-valid images: `../../patches/e46_baud38400.bin`,
`../../patches/e46_baud125000.bin` (+ `.json` manifests).

### Toolkit support (this change)

`dde3-toolkit baud [--rate 38400|125000]`:
- sends `12 08 91 00 <baud24 BE> 03` at 9600,
- reopens the port at the ECU's actual divisor (125000 → **124800**),
- drains the ACK (which arrives at the new rate — the S0BG write precedes the
  ACK queue), and verifies with IDENT; on failure it falls back to 9600 to tell
  "never switched" (stock/unpatched) from "dead" (power-cycle).

## 5. Fastest live logging — the numbers

At 9600 8E1 a byte is ~1.04 ms. The measured real-car baseline was **2.5 Hz**
(400 ms/cycle, 5 ids) — dominated by the old 55 ms poll sleep + 3 ms/byte TX
pacing, not the wire. Tier 0 (sleep removed, 1 ms/byte live TX) already helps.

Per-cycle wire cost for a 5-id poll (TX 7 B + RX 16 B = 23 B):

| rate | wire time | + ECU turnaround | plausible Hz |
|---|---|---|---|
| 9600 | ~24 ms | + ECU task latency | ~5–10 Hz (tester-paced today: 2.5) |
| 38400 | ~6 ms | + ECU task latency | ~15–30 Hz |
| **124800** | **~2 ms** | **+ ECU task latency** | **~30–50 Hz** |

Above ~50 Hz the **ECU's own measurement loop** (order 5–20 ms, the 0x2EF4 task
scalars) caps freshness no matter the baud — more wire speed then buys nothing.

**Recommended logger flow (to implement on the build host):** add `--fast-baud
<rate>` to the `bench`/GUI live path:
1. open 9600, unlock (UPROG_EIN + seed/key), send IDENT to confirm;
2. `baud --rate <rate>` inline (switch + reopen at the actual divisor);
3. run the existing batched `0B 10` live loop at the new rate;
4. on exit, leave the ECU (it reverts to 9600 on the next power cycle).

This needs **the baud patch flashed** (`e46_baud125000.bin`). Without flashing,
9600 Tier-0 (~5–10 Hz) is the ceiling — the stock ECU has no in-band DS2 baud
change (service 0x91 absent) and transmits nothing spontaneously, so there is
no listen-only fast path.

**TX pacing is zeroed at the fast rate** (all bench modes, matching the GUI
write job): the 3 ms/byte WinKFP default is a 9600-baud habit — at 124800 a
73-byte `FLASH_SCHREIBEN` frame would take 219 ms of pacing alone vs 5.9 ms
of wire, so a full write stays ~11.5 min instead of ~36 s. The request/reply
structure still gives the ECU a full ACK round-trip between frames, so
back-to-back is safe; the master never runs more than one frame ahead.

## 6. Status checklist

| item | status |
|---|---|
| DS2 service→handler map | verified (disasm) |
| 0x06/0x07 are app-mode NACK stubs; flash needs programming mode | verified (disasm) |
| WinKFP flash frame templates | extracted (job decompile) |
| flash read/write frame layout (count width, endianness) | **RESOLVED (VM + bytecode): 1-byte count, big-endian addr; code fixed** |
| batched live `LEN=5+2n`, 0x12 reply prefix, checksum status `12 05 A0 00` | confirmed on real car |
| baud ladder + 125000 = S0BG 0x03 | verified (disasm + arithmetic) |
| baud patch (38400 + 125000), checksum-valid, correct jump encoding | built + verified offline; **bench test pending** |
| `baud --rate` toolkit support | code added (build on host) |
| fast-baud logger flow | **implemented**: headless (`bench --live --fast-baud`) + GUI Live-tab "Fast baud (125k)" toggle (adversarially reviewed; builds on host) |
