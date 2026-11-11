# EDC15M / DDE3.0 — K-line baud rates, DS2 live-data batching, and the
break-entered service protocol

Source: `edc15m_out/research/out/disasm.txt` (C166 disassembly),
`edc15m_out/research/routines_part3.md` (DS2 command map),
`decompiled_winkfp.txt` / `decompiled.txt` (WinkFP job listings),
`testo customjobs.txt` (**tested, working** WinKFP logging jobs — the
ground-truth reference), and the confirmed wire captures in `HANDOVER.md` §15.

All addresses are C166 program-memory unless noted.

---

## 1. The K-line is one UART with a three-value baud ladder

The K-line is the C166 **S0** serial port. Every baud change in the entire
firmware is a write to the baud-generator register `S0BG`. There are exactly
**three** values anywhere in the image:

| `S0BG` | Baud | Where |
|--------|------|-------|
| `0x33` | **9600** (default) | `0x1E8EE` — UART init; the only default. Confirmed by `xsetpar 80 25` (9600 8E1). |
| `0x2F` | ~10.4 k | `0x1EC06` (state 3), `0x1ED54` (state 6) |
| `0x0C` | ~38.4 k | `0x1ED66` (state 6, only when `C332 & 0xFF00 == 0xA600`) |

Format is **8 data bits, even parity, 1 stop** (`9600 8E1`).

**Key consequence:** the diagnostic (DS2) protocol runs at **9600 and the
firmware never re-rates the port while doing DS2 traffic** (see §2). 10.4 k /
38.4 k exist *only* on the separate break-entered channel (§4).

## 2. The DS2 diagnostic protocol is fixed at 9600 — the length-byte rule

The DS2 service set (`00` ident, `04/05` fault, `06/07` flash, `0B` MW live,
`0D` uprog, `0A` checksum, `90` seed-key, …) is dispatched by the task at
`0x290D4` (routines_part3) into the 128-byte buffer at `C6C9h`, served by the
S0 receive state machine at `0x1E904`. States 0–2 (idle / receive / transmit)
of that machine **never touch `S0BG`** — the port stays at 9600. There is no
in-band DS2 command that re-rates the K-line.

### 2.1 The trailing-XOR frame and the length byte

On the wire a frame is `<payload> <XOR>` (no B8 prefix, no tester address —
confirmed by capture, HANDOVER §15). The receiver:

- byte[0] must be `0x12` (DDE address) — checked at `0x1EA86`.
- `C6C8h` is the byte counter, starts at 1 (addr byte) at `0x1EA68`, and is
  incremented for every byte received (`0x1EB22`).
- when `C6C8 == 2` (the 2nd byte = the length byte) the length is latched into
  `C6C7` at `0x1EB2C` and validated to be in **3..0x80** (`0x1EB30`–`0x1EB38`).
- the frame is *complete* when `C6C8 == C6C7` (`0x1EB46`), i.e. **the length
  byte equals the total number of bytes received, including the address byte
  AND the trailing XOR byte.**

So the universal rule is:

```text
LEN = total wire bytes = (frame bytes before the XOR) + 1 (the XOR byte)
```

The 0x80 (128) cap means a single DS2 frame carries at most ~120 data bytes.

### 2.2 The working reference: `MW_SELECT_LESEN_NORM` (tested logger)

`testo customjobs.txt` is a **tested** WinKFP logging job set — the exact
thing that logs correctly on the real ECU. Each job is:

```xml
<customjob ecu="DDE30DS0">
  <jobname>MW_SELECT_LESEN_NORM</jobname>
  <virtualname>SET_3</virtualname>
  <comment>Live</comment>
  <argument>0F630F620F610F600F650F000F300F100E810F800F8A0F840F520F530F40</argument>
</customjob>
```

The `<argument>` is a flat, packed list of diagnostic IDs (2 hex bytes each =
one u16 id). `SET_3` above = **15 ids in one telegram**.

The disassembly of the job (`decompiled.txt` @ `0x5FE5F`) shows how the DS2
telegram is built:

```text
0x5FEE2  move  S1, {12 05 0B 10}     ; fixed prefix (LEN slot is a placeholder)
0x5FFB9  slen  L0, S6               ; L0 = strlen(arg_bytes)
0x5FFC9  move  L0, #$5              ; ...
0x5FFD9  adds  L0, L1               ; total = arg_len + 5
0x6000B  scat  S3, S6               ; telegram = prefix ++ arg_bytes
0x60041  xsend S2, S3
```

So the **length byte is computed at runtime as `5 + arg_len`** (= 4-byte prefix
`12 LEN 0B 10` + arg bytes + 1 XOR). This is exactly the §2.1 rule. The `05`
literal in the prefix is a placeholder overwritten by the `slen`+5 arithmetic —
**not** a fixed constant. A frame of `n` ids therefore is:

```text
12 (05 + 2n) 0B 10 <id0_hi id0_lo> ... <id(n-1)_hi id(n-1)_lo>  <XOR>
```

(Length is a u8 capped at 0x80, so a single telegram holds up to (0x80−5)/2 =
**118 ids** — far more than any live set.)

> **The toolkit bug:** `frame_mw_select` in `src/protocol.rs` hard-coded the
> length byte to `0x05` and the live loop chunked ids in groups of 10. With the
> correct rule the length must be `5 + 2·n`. The in-process sim ignored the
> length byte (it counts ids from the payload), which is why batching "worked"
> against the sim but a multi-id frame would be mis-counted by the real ECU.
> **Fix applied this session:** compute `LEN = 5 + 2·n` and send the whole
> selected set in one telegram.

### 2.3 Reply layout for a batched MW read

ECU reply (echo style, see HANDOVER §14 / `sim.rs`):

```text
92 05 0B 10 A0 <v0_hi v0_lo> <v1_hi v1_lo> ... <v(n-1)>  <XOR>
```

— one big-endian u16 per requested id, in request order, after the
`92 05 0B 10 A0` header (data starts at offset 5). The values are raw counts
that the *tester* scales via `BetriebswTab` (the job continues into `tabset
"BetriebswTab"` after the xsend), i.e. the toolkit's `FACT_A/FACT_B` scaling in
`src/live.rs`.

### 2.4 Open finding — flash read/write length bytes (needs HW check)

`tools/analysis/frames.md` (VM-extracted ground truth) shows the flash service
frames as:

```text
12 09 06 <A_lo> <A_mid> <A_hi> <count_lo> <count_hi>     ; FLASH_LESEN, LEN=09 (8 bytes + XOR)
12 09 07 02 <A_lo> <A_mid> <A_hi> <data...>              ; write
```

i.e. the read **count is 2 bytes** and the frame length is `0x09` for the
minimal form. The toolkit's `frame_flash_read`/`frame_flash_write` currently
emit a **1-byte** count and a hard-coded `LEN = 0x09`. That is consistent with
the reference only for the minimal frame, and would violate the §2.1 rule for a
0x80-byte write block (payload+XOR would exceed the 0x80 cap). Yet HANDOVER §18.1
records a full 256 KB write completing on hardware. **This contradiction is
unresolved and must be checked against a fresh capture before any flash framing
is changed.** No flash-path change was made this session on purpose.

## 3. `BAUDRATEN_*` jobs are a red herring for K-line speed

- `BAUDRATEN_UMSTELLUNG` (`10DDE30.prg`) sends DS2 service `0x91`
  (`12 08 91 00 25 80 03`); `0x91` is **not** in the DS2 dispatch chain at
  `0x290D4`, and `SET_EDIC_BAUDRATE` targets the **CAN / engine bus ("EDIC")**,
  not the K-line.
- `BAUDRATEN_LESEN` sends `12 04 0D` to *read* the current baud.

Neither makes DS2 traffic faster. Correctly dropped from the toolkit.

## 4. The break-entered service protocol (the *other* protocol world)

The 10.4 k / 38.4 k bauds are used by a **separate, byte-oriented channel** in
states 3–6 of the *same* S0 state machine at `0x1E904`. It is entered by a UART
**break** while the DS2 link is idle, not by a DS2 service byte.

### 4.1 Entry sequence (grounded in the disassembly)

| State | Addr | Action |
|-------|------|--------|
| →3 | `0x1EA44` | A frame-error / break condition sets `C749 = 3`. |
| 3 | `0x1EC06` | `S0BG = 0x2F` (**~10.4 k**), `S0CON = 0xD4`, → state 4. |
| 4 | `0x1EC42` | Wait for a byte whose high nibble is `0xA` (`0x1EC60`–`0x1EC68`: `AND #F0h; CMP #A0h`). Only accepted when the unlock preconditions hold: `C0DFh.0` set **and** `C3A9h == 0xA5` (`0x1EC70`–`0x1EC90`). On success → state 5. The first handshake byte is **`0xA5`** (it matches the `C3A9 == 0xA5` unlock value). |
| 5 | `0x1ECB6` | **Second** handshake byte: latched into the handshake word `C332h` (high byte) and the PEC register `FEB0h = C332h \| 0x100` (`0x1ECD2`–`0x1ECDA`), echoed back, → state 6. **`0xA6` selects the fast rate** (see state 6); any other byte keeps 10.4 k. |
| 6 | `0x1ED4C` | `S0BG = 0x0C` (**38.4 k**) **only if** the handshake word `C332h & 0xFF00 == 0xA600` (`0x1ED46`–`0x1ED64`); otherwise the port stays at 10.4 k (`0x2F`). |
| →0/51 | `0x1F6D2` | Secondary service protocol — the `0x51`-entry jump-table interpreter (byte-addressed RAM / EEPROM / PEC bulk access, part 2 §5). |

### 4.2 Practical implications

- **The port must be re-armed by the tool** on every baud change (`set_baud_rate`
  + drain), because both sides change rate — same pattern as
  `Kline::set_baud` in `src/protocol.rs`.
- **Entry is not free:** the unlock preconditions (`C0DF.0`, `C3A9 == 0xA5`) are
  internal ECU flags set by prior session state (programming/unlock via the DS2
  seed-key / uprog path). The tool must first unlock, then break-enter.
- The **38.4 k** rate applies to a *different framing model* (byte-address
  command/response, no DS2 `LEN`/`XOR` envelope) — a real second protocol, not
  a baud knob on the DS2 path.

### 4.3 Plan to implement it in `dde3-toolkit`

New module `src/breakmode.rs`:

1. **Baud-ladder constants** from §1 (`0x33`/`0x2F`/`0x0C` → 9600 / 10.4 k /
   38.4 k) with a documented `baud_for_s0bg()` mapping.
2. **`BreakMode::enter(kline)`** driving the K-line side of §4.1:
   - hold the line low for a break (longer than one 9600 frame), release;
   - set the tool to **10.4 k**, drain, send the **`0xA5` handshake byte**;
   - read the echoed byte, then set the tool to **38.4 k** when the response
     indicates the `0xA6` high byte;
   - return the secondary-protocol handle.
3. **Raw byte transport** (`write_bytes` / `read_bytes` at the current rate) —
   the primitive the secondary commands will use once their opcodes are decoded.
4. **In-process sim** (`BreakSim`) walking the handshake so the entry path is
   unit-testable offline.
5. **CLI**: `dde3 break-probe` to enter the mode and dump the first secondary
   response for `tools/monitor.py` capture; a `tools/break_capture.py` hook.
6. ~~Secondary command opcodes~~ — **decoded** (see §4.4); implemented as
   `opcode::*`, `request()`, `parse_reply()`, `BreakHandle::command()` and the
   `BreakSim` model in `src/breakmode.rs`.

> Status: the *entry state machine* (§4.1) **and** the *secondary command set*
> (§4.4) are implemented in `src/breakmode.rs` and grounded in the disassembly.
> What remains capture-pending is the **on-wire layout** of the 4-byte frames
> (byte order on the K-line, whether the ECU echoes, exact PEC framing of the
> `0x3E` bulk stream) — the mailbox-level behaviour below is from the disasm.

### 4.4 Decoded command format and opcode map (verified against `out/disasm.txt`)

The state-6 command path is an interpreter **task** at `0x1F6D2` (initialised
by `0x1F6A0`), dispatched through a 0x51-entry jump table at `0x1F724`.
Slot = `(table_addr − 0x1F724) / 4`; the opcode byte is clamped to `0x50`.

**Frame shape** — raw 4-byte blocks, no DS2 length byte, no trailing XOR:

```text
request : [0x01, OPC, xx, DATA]   first byte MUST be 0x01 (gate at 0x1F6EC)
reply   : [0x00, TAG, RESP, HI]
          TAG = 0x02 normal | 0x01 PEC-not-ready | 0x05 busy (PEC bulk active)
          HI  = 0x01 data replies | 0x00 the two unknown-opcode defaults
```

The request lands in the mailbox `C32E..C331`; the interpreter copies it to
`C0FA..C0FD` (opcode = 2nd byte, `DATA` = 4th byte; the 3rd request byte is
ignored by every handler). Replies are written to `C0FA..C0FD` (`0x1FC26`) and
mirrored back to the mailbox for the PEC transmit path.

**Opcode map** (slot → handler, all verified against the table at 0x1F724):

| opc | handler | action |
|-----|---------|--------|
| 0x02 | 0x1F868 | echo `DATA`; `C887 += DATA` |
| 0x04 | 0x1F886 | reply address low byte `C804`; `C887 += C804` |
| 0x07 | 0x1F8A4 | reply address high byte `C805`; `C887 += C805` (handler text is byte-identical to 0x04's in the disasm) |
| 0x0B / 0x0E | 0x1F8C2 | word-aware RAM read at `C804`; `C804 −= 1` (24-bit) |
| 0x0D | 0x1F8CA | RAM read at `C804 + C1B8` (offset word at RAM 0x01B8) |
| 0x10 | 0x1F8DC | read block buffer `C80E[C804]` if `C804 < C802` else `0xEE`; `C804 −= 1` |
| 0x13 | 0x1F90E | fault-memory (DTC record) read at `C804 + 0xCE7A` |
| 0x18 | 0x1F91A | 16-bit read at `C0B8 + 2·C804 − 2` (EEPROM mirror); `C804 == 0` answers `8` |
| 0x1B | 0x1F938 | reply checksum accumulator `C887` |
| 0x25 | 0x1F952 | `C804 ← DATA` (low byte); answer `DATA XOR OPC` |
| 0x26 | 0x1F976 | `C805 ← DATA` (high byte); answer `DATA XOR OPC` |
| 0x2A | 0x1F99A | block handshake/reset: `DATA` must be `0x11` (answer `0x2A`, else `0xE1` without touching state); resets `C802 = 0x12`, `C887 = 0`, fills the 0x50-byte `C80E` buffer with the `1C FF` pattern, zeroes partner cache + address high byte |
| 0x31 | 0x1FA0E | store `DATA` → `C80E[C804]` if in range else `0xEE`; `C804 −= 1`; answer `DATA XOR OPC` |
| 0x3E | 0x1FA46 | PEC bulk transfer: first pass arms the PEC channel; the `0x01AA → 0x0155` handshake and the S0-block stream are wire-level (capture-pending) |
| 0x4F | 0x1FB76 | `DATA` must be `0x3D`/`0x3E` else `0xE1`; sets `C802 = 2·[RAM 0x0E9A]` |

**Unknown opcodes** answer through two default slots — **`0xED`** (default A,
`0x1FBBA`) or **`0xEB`** (default B, `0x1FBD0`) depending on the table slot —
with `HI = 0x00` (the default handlers don't set the `0x01` data tag).
The per-slot A/B assignment is transcribed in `BreakSim::default_reply`.

**Word-aware RAM read** (shared routine `0x1FBE6`): the address register is
the 24-bit value at `C804..C806`; the **partner cache** is the 24-bit value at
`C806..C808` — the two **share byte `C806`**. An even-address read fetches the
word, caches the high byte in `C88A`, stores `C806..C808 ← addr − 0x0FF1E`
(24-bit) and answers the low byte; a later read at the partner address answers
the cached high byte; a miss clears the partner cache and does a plain byte
read. Every read does `C804..C806 −= 1` (which ripples into the shared
partner byte), i.e. pre-decrement pointer semantics.

**Corrections to `research/routines_part2.md`:** the checksum opcode is
**`0x1B`** (not 0x1C) and the 16-bit EEPROM read is **`0x18`** (not 0x19) —
the jump-table slots place them there.

**CLI:** `dde3 break-probe` now drives the full entry (§4.1) and then sends the
verified sequence `0x2A/0x11` (block handshake), `0x1B` (checksum),
`0x04`/`0x07` (address) and the legacy `0x51` probe (expected default-A reply
`00 02 ED 00`) so a hardware capture can confirm the on-wire layout.

## 5. What this means for "the fastest rates"

- **DS2 read / write / log ceiling = 9600 8E1.** No firmware switch makes
  `FLASH_LESEN` or MW logging faster; the toolkit is already at that ceiling.
- **The real lever is fewer round-trips.** Batching live data into one
  `0B 10` telegram (this change) cuts N value round-trips to 1.
- **The break-entered 38.4 k channel** is the only faster link, but it is a
  separate protocol for bulk byte access, not a faster version of the DS2 live
  path — hence the plan in §4.3.
