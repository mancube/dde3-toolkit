# UPROG_EIN (`0x0D`) — what it is and what it's used for

**2026-11-22.** Deep-dive on the real-car `UPROG_EIN` reply, traced through
the ECU firmware decompilation (`~/Desktop/edc15m/research/out/disasm.txt`,
C166) and the real-car capture (`captures/actual ecu UPROG_EIN response.txt`).
Supersedes the brief note in `docs/archive/SESSION_2026-11-19.md` (archived;
kept for the session context) and corrects one of its claims (see
["Correction to the 2026-11-19 note"](#correction)).

## TL;DR

`UPROG_EIN` is the DS2 command `12 04 0D` (service `0x0D`, "UPROG" =
*UnterProgram* / enter programming). It does two things at once:

1. **Opens the flash/programming session.** In the normal application session
   the flash services (`0x06` read, `0x07` write/erase) are NACK stubs
   (`12 04 A2`). After `UPROG_EIN` **and** seed/key, real flash read/write
   works.
2. **Returns a 72-byte "region/segment descriptor table"** — a list of 24-bit
   addresses pointing at the ECU's identification/data blocks (serial numbers,
   HW/SW codes, the AIF record). This is the map a tester (MPPS) uses to know
   *where* to read those blocks.

It is also the command the **PRUEFCODE_LESEN** (test-code) job uses
(`diag_jobs.md` #165), so the same reply feeds the ECU's Prüffcode (security /
checksum code).

## The command and dispatcher

The DS2 dispatcher at `0x2932C–0x293D0` compares the command byte (`R4`) and
branches. For `0x0D` it jumps to the UPROG_EIN handler:

```
029368  CMP  R4,#0x0D        ; service == 0x0D ?
02936C  JMPA cc_Z,0x2954C    ;   -> UPROG_EIN handler
```

The handler at `0x2954C` is a thin wrapper (the same shape as every other
service handler — common prologue, one logic call, common reply tail):

```
02954C  CALLS 0x2692C   ; common prologue (stack check)
029550  CALLS 0x277A0   ; <- the actual UPROG_EIN logic
029554  JMPR  0x295F6   ; common reply tail
```

## The handler (`sub_0277A0`), decompiled

```
0277A0  CMP   R0,FD86h        ; stack-depth check (reset code 0x25 on overflow)
0277A4  JMPR  cc_NC,0x277B0
0277A6  BCLR  PSW.11          ;   ...overflow path: reset
0277A8  MOV   MDL,#0x25
0277AC  JMPS  0x276
0277B0  MOVB  RL4,CD42h       ; RL4 = frame length (set by the dispatcher)
0277B4  CMPB  RL4,#4          ; gate: must be a 4-byte (no-argument) command
0277B6  JMPA  cc_NZ,0x27826   ;   else RETS with no data (ignored)
0277BA  MOV   R4,#0x48        ; reply data length = 72
0277BE  MOV   CD60h,R4        ;   [CD60] = 72
0277C2  MOV   R12,#0xCBF B   ;   dest  = reply buffer
0277C6  MOV   R13,#0x7C6A     ;   source = MCU 0x7C6A  (= flash 0x3FC6A)
0277CA  MOV   R14,R4          ;   count  = 72
0277CC  CALLS 0x26A2          ; copy 72 bytes (boot-block memory reader)
0277D0  MOVB  RL4,#3          ; assemble a 3-byte value [0x03, hi, lo]:
0277D2  MOVB  CC28h,RL4       ;   CC28 = 0x03
0277D6  MOV   R4,[0xE96]      ;   R4 = [0xE96] + 0x8000  (a RAM variable)
0277DA  ADD   R4,#0x8000
        ... R4 hi -> CC29, R4 lo -> CC2A ...
027800  MOV   R4,#4
027802  ADD   CD60h,R4        ; total reply length = 72 + 4 = 76
02780A  MOVB  CBF9h,RL4       ;   [CBF9] = 76 (LEN field)
02780E  MOVB  RL5,#0xA0       ; status = OKAY
027812  MOVB  CBFAh,RL5
027826  RETS
```

Reading it:

- **Gate `[CD42] == 4`.** `CD42` is the incoming frame length (set in the
  dispatcher before the command branch). `UPROG_EIN` takes no parameters, so
  the handler only acts when the frame is exactly the 4-byte no-arg form
  `12 04 0D <XOR>`. A longer frame is ignored. (The sibling no-arg handlers —
  `0x0E`/`0x0F` test-stamp, etc. — gate on the same value.)
- **Copy 72 bytes from MCU `0x7C6A`.** The calibration area is the last
  `0x8000` of the 256 KB flash, so MCU `0x7C6A` = flash `0x38000 + 0x7C6A` =
  **`0x3FC6A`**. `0x26A2` is the same boot-block memory reader the AIF read
  (`0x02`) uses (it copies the 46-byte AIF slot for that command). It is in
  the boot block (`0x00000–0x07FFF`), which is **not in the dump**, so its
  body isn't disassembled here — only its effect (a `src -> dest` byte copy,
  `R13 = src`, `R12 = dest`, `R14 = count`).
- **Runtime triple at offset 45.** The firmware assembles a 3-byte value
  `[0x03, hi, lo]` from the RAM variable `[0xE96] + 0x8000` and places it at
  byte 45 of the reply, overwriting the three `FF` bytes that are there in
  flash. On the captured car this is `03 AE BE` (so `[0xE96]+0x8000 = 0xAEBE`).
- **Reply** = `12 4C A0 <72 data bytes> <XOR>` (LEN `0x4C` = 76 wire bytes
  including the trailing XOR).

## The 72-byte reply: a region/segment descriptor table

The real-car reply (`captures/actual ecu UPROG_EIN response.txt`):

```
12 4C A0
03 FF 60
FF FF FF ... FF            (42 x FF)
03 AE BE
00 7F E4  03 FF 46  03 FF 48  03 7F B0  03 FC B2  03 FF 47  03 FC 4F  03 FF 62
F0                          (XOR)
```

Laid out by offset:

| off | bytes | value | meaning |
|---|---|---|---|
| 0   | `03 FF 60` | `0x03FF60` | first table entry |
| 3   | 42 × `FF` | — | padding |
| 45  | `03 AE BE` | `0x03AEBE` | **runtime** (FF in flash; `[0xE96]+0x8000`) |
| 48  | `00 7F E4` | `0x007FE4` | table entry |
| 51  | `03 FF 46` | `0x03FF46` | table entry |
| 54  | `03 FF 48` | `0x03FF48` | table entry |
| 57  | `03 7F B0` | `0x037FB0` | table entry |
| 60  | `03 FC B2` | `0x03FCB2` | **the AIF slot** |
| 63  | `03 FF 47` | `0x03FF47` | table entry |
| 66  | `03 FC 4F` | `0x03FC4F` | table entry |
| 69  | `03 FF 62` | `0x03FF62` | table entry |

The last 24 bytes (the eight `00 7F E4 … 03 FF 62` addresses) are a **pure
flash window** at `0x3FC9A..0x3FCB2` — verified byte-for-byte against
`actual_flash.bin`, ending exactly at the AIF slot (`0x3FCB2`). The first 48
bytes are `03 FF 60` + FF padding, with the firmware overwriting bytes 45–47
with the runtime `03 AE BE` triple.

What the addresses point at (in `actual_flash.bin`):

| addr | content |
|---|---|
| `0x03FF46` / `0x03FF47` / `0x03FF48` | ASCII serial / part numbers (`00005331…`) |
| `0x037FB0` | ASCII `00005331C5…` |
| `0x03FCB2` | the 46-byte AIF programming record |
| `0x03FC4F` | ASCII HW code (`03000281001445…`) |
| `0x03FF62` | binary |

So the table is a **map of where the ECU's identification data lives**. A
tester reads the addresses in it to populate its display — this is exactly the
"region-table pre-read" MPPS performs after `UPROG_EIN` (it reads
`0x03FF46/47/48`, `0x037FB0`, `0x03FCB2`, … to show serial numbers, HW/SW
codes and the AIF record). That pre-read is **UI data, not a protocol
requirement** — the toolkit does not need to send it.

## What it's used for

1. **PRUEFCODE_LESEN (test-code read).** The job catalogue
   (`research/diag_jobs.md` #165) lists `12 04 0D` inside the test-code
   sequence: `IDENT (12 04 00)` → `EEPROM byte 0 (12 09 06 06 00 00 00 01)` →
   **`UPROG_EIN (12 04 0D)`** → `fault reads (12 05 04 01 / 12 05 14 01)` →
   returns the **PRUEFCODE**. The `0x0D` reply is one of the inputs to the
   ECU's Prüffcode (security / checksum code).
2. **Entering programming mode.** Confirmed by the 3-command capture: after
   `UPROG_EIN` but *before* seed/key, `FLASH_LESEN` still gets the `12 04 A2`
   NACK stub. Real flash access needs `UPROG_EIN` **and** seed/key.
3. **Region map for the tester.** See above — MPPS uses the table to find the
   identification blocks. Not required by the toolkit.

## Relation to the toolkit

`Kline::begin_flash_session()` sends `UPROG_EIN` (`12 04 0D`) as the second
step of the flash bootstrap (after IDENT, before the AIF read and seed/key).
The toolkit only needs the *session-open* effect of the command — it does not
parse the 72-byte table. The fake ECU (`src/sim.rs`) serves the captured
72 bytes verbatim (`UPROG_EIN_DATA`), which is correct because the runtime
triple is car-specific and the rest is a stable flash window.

## Correction

`docs/archive/SESSION_2026-11-19.md` states: *"The 72 bytes are **not stored in
flash** — not found anywhere in `actual_flash.bin`."* The decompilation
refines this. The handler **does** copy 72 bytes from flash `0x3FC6A`
(MCU `0x7C6A`), so the table's structure *is* in flash. The full 72-byte match
against the dump fails **only** because the firmware overwrites bytes 45–47
(`ff ff ff` → `03 AE BE`) at runtime. So "not in flash" is true of those three
runtime bytes, not of the whole table. The last 24 bytes are a verbatim flash
window at `0x3FC9A..0x3FCB2`.

## Sources

- `research/out/disasm.txt` — `sub_0277A0` (UPROG_EIN logic), dispatcher
  `0x2932C–0x293D0`, `0x26A2` call sites.
- `research/diag_jobs.md` — #165 `PRUEFCODE_LESEN`, the `12 04 0D` entry.
- `research/FIRMWARE_REPORT.md` §C — service map (`0x0D → 0x2954C`), the
  programming-mode caveat (flash services are stubs until `UPROG_EIN`).
- `captures/actual ecu UPROG_EIN response.txt` — the real-car 76-byte reply.
- `captures/actual_flash.bin` — flash `0x3FC6A..0x3FCA2` (source),
  `0x3FC9A..0x3FCB2` (region-table window), `0x3FCB2` (AIF slot).
- `src/sim.rs` — `UPROG_EIN_DATA`, the `0x0D` handler, the session-gating
  tests.
