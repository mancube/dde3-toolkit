# DS2/K-line receive buffer & frame-size limits (firmware-confirmed)

> Answers one question from the EDC15M (DDE3) C166 firmware: **what is the
> largest frame the ECU will accept on the K-line?**
>
> Short answer: **128 bytes in the normal session; larger (≥ 233, exact size
> unconfirmed) in programming mode.** The often-quoted "256" is **not
> confirmable** — the boot block that would hold it is absent from every dump.
>
> Source image: `e46_orig.bin`. Disassembly: `research/out/disasm.txt`.
> Written analysis: `research/routines_part2.md`, `research/FIRMWARE_REPORT.md`.

## TL;DR

| Receiver | Where | Max frame (`LEN`, total incl. trailing XOR) | Status |
|---|---|---|---|
| Application DS2 receiver | `0x1E904` (app code, **in** dump) | **`LEN ≤ 0x80` = 128** | **confirmed** from disassembly |
| Programming-mode flash programmer | boot block `0x0000–0x7FFF` (**not** in dump) | **≥ 233** (MPPS proves it); 256 unconfirmed | **not confirmable** (boot block all `0xFF`) |

The 128-byte gate is the real ceiling for **normal diagnostics**. The
233-byte MPPS calibration-write frames work only because they are sent in
**programming mode** (after `UPROG_EIN`), where the boot-block programmer —
with a bigger buffer — takes over the K-line.

## 1. The application DS2 receiver: 128 bytes (confirmed)

The K-line is driven by a single C166 S0 UART. In the application there is
exactly **one** receive path — the byte-wise state machine at `0x1E904`
(state in `C749`), which contains the **only** `S0RBUF` read in the app code
(`0x1E994`).

### The buffer

| Fact | Instruction |
|---|---|
| Buffer base | `0x1EA4A  MOV R0,#C6C9h` |
| Buffer end (bound check) | `0x1EB08  CMP R0,#C748h` |

The telegram buffer is **`C6C9..C748` = 128 bytes**. The write pointer (`C18A`)
is compared against `C748` before each store; a pointer at/past the end flags
the frame bad.

### The length gate

The second byte of every frame is the total length `LEN` (address + payload +
trailing XOR). When the byte counter reaches 2 the receiver validates it:

```
01EB2C  MOVB  C6C7h,RL1      ; C6C7 = LEN byte (2nd byte of frame)
01EB30  CMPB  RL1,#3         ; minimum
01EB32  JMPR  cc_C,  bad     ; LEN < 3   -> bad frame
01EB34  CMPB  RL1,#80h       ; maximum
01EB38  JMPR  cc_ULE, ok     ; LEN <= 0x80 -> ok
01EB3A  BSET  FDD6h.3        ; LEN > 0x80 -> BAD FRAME flag
```

So the application receiver accepts **`3 ≤ LEN ≤ 0x80` (128)** and drops
anything larger. There is **no `0x100`/256 constant** anywhere in the DS2
module (`0x1E8DA–0x1F085`).

This matches the written analysis in `research/routines_part2.md` §4 ("the
second byte … at most 0x80 … buffer `C6C9..C748` holds 128 bytes").

## 2. Why the 233-byte MPPS frames work: a second, bigger receiver

The MPPS-style calibration write sends each 224-byte block as one **233-byte**
frame (`12 E9 07 02 <addr3> E0 <224 data> <XOR>`, `LEN = 0xE9`). That exceeds
the 128-byte app limit, yet it writes with 100% on-car success
(`captures/mpps capture/mppswrite.txt`). The two facts are reconciled by a
**two-receiver split**:

- In the **normal session** the flash handlers are **stubs**: `0x06` read at
  `0x26DA2` and `0x07` write/erase at `0x26DBC` both begin with the same stack
  check (`CMP R0,FD86h`) and reply **`0xA2` (NACK)** without touching flash.
- Real flash I/O only works **after `UPROG_EIN` (service `0x0D`)** enters
  programming mode. The MPPS capture shows the order: `12 04 0D` UPROG_EIN @
  14.7 s → … → the `12 E9 07 02` 233-byte frames @ 38.6 s.
- The programming-mode flash programmer lives in the **boot block
  (`0x0000–0x7FFF`)** and drives the same UART with its **own** receive buffer.
  That is the receiver that accepts the 233-byte frames.

So the 128-byte gate at `0x1EB34` never sees the MPPS write frames — they are
handled by the boot-block programmer, not the app state machine.

## 3. The "256" theory: not confirmable (boot block missing)

The boot block is **all `0xFF` in every available image**, including the
real-car full read:

| Image | Boot block `0x0000–0x07FFF` |
|---|---|
| `binaries/e46_orig.bin` | all `0xFF` (0 non-`FF` bytes) |
| `binaries/WBAAL71050CA52046_DDE3_7786887_fullread.bin` | all `0xFF` (0 non-`FF` bytes) |

So the programming-mode programmer — where a 256-byte buffer would live —
**cannot be analyzed**. From the evidence we can only bound it:

- **≥ 233 bytes** (MPPS's 233-byte frames are accepted on-car).
- **256 is plausible but unconfirmed** — nothing in the available disassembly
  proves or refutes it.

To confirm the exact size you would need a boot-block image (a JTAG/BMS read of
`0x0000–0x07FFF`, or a Bosch library dump) — the K-line read does not expose it.

## 4. Practical implications for the toolkit

- **Normal diagnostics** (ident, AIF, DTC read/clear, seed/key, live data, AIF
  read): the hard ceiling is **128 bytes total** (`LEN ≤ 0x80`). Everything the
  toolkit sends in the normal session is well under that — no action needed.
- **Calibration/flash write** (`write_calibration` / `bench --write-cal`): must
  send `UPROG_EIN` **first** (it does, via `begin_flash_session`). That routes
  the 233-byte blocks to the big boot-block buffer. **Keep it that way** — a
  >128-byte frame sent in the *normal* session would be dropped at `0x1EB34`.
- Do **not** raise any toolkit-side assumption to 256 on the strength of the
  MPPS frames: those only prove the *programming-mode* buffer, which is a
  different receiver from the 128-byte app one.

## Sources

- Disassembly: `research/out/disasm.txt` — state machine `0x1E904–0x1F085`;
  buffer base `0x1EA4A`; buffer bound `0x1EB08`; length gate `0x1EB30–0x1EB3A`;
  sole `S0RBUF` read `0x1E994`; app flash stubs `0x26DA2`/`0x26DBC`.
- Written analysis: `research/routines_part2.md` §4 (DS2 receiver),
  `research/FIRMWARE_REPORT.md` §C.2 (flash stubs) & §C.3 (tester flash frames).
- Capture: `captures/mpps capture/mppswrite.txt` (UPROG_EIN @ 14.7 s, 233-byte
  frames @ 38.6 s).
- Images: `binaries/e46_orig.bin`,
  `binaries/WBAAL71050CA52046_DDE3_7786887_fullread.bin` (boot block all `0xFF`).
