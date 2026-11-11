# DS2 service 0x91 (baud change) — ECU patch + `baud` command (session 8)


Stock firmware does **not** implement DS2 `0x91` (no case in the dispatcher
chain 0x2933E–0x293D0, `research/out/disasm.txt`; falls through to the
unsupported path 0x295F2 → reply `12 04 FF`). The stock tester job
`BAUDRATEN_UMSTELLUNG` (10DDE30.prg @ 0x121C, template `12 08 91 00 BBBB
BBBB BB 03`) was never wired to it either — that is why nothing in the
stock DDE30 world can re-rate the DS2 link.

**ECU-side patch** (32 bytes, no TDI checksum impact):
- dispatcher hook 0x293D0: `EA 00 F2 95` → `EA 00 C3 18` (fallthrough now
  targets trampoline T @ 0x185C3 — a 65-byte erased, zero-reference block)
- T: `CMP R4,#91; JMPA cc_Z,H; JMP 0x295F2`
- H: `CALLS 0x2692C` (header) + `CBFA=A0` (ACK) + `MOV S0BG,#0x0C` (38.4 k)
  + `JMP 0x295F6` (common tail) — exact idioms of the 0x9E case and the
  S0BG encoding at 0x1E8F4.

Key wire semantics: the ACK is queued **after** the S0BG switch, so it
goes out at 38.4 k — that is the sync point; never expect a reply at the
old rate. Revert to 9600 = power cycle (v1 hard-codes 38.4 k; variant V2
honors the 24-bit baud field from CBF6+1..+3).

Full docs + applier + patched image:
`~/Desktop/edc15m_out/baud91_patch/{patch_0x91.py, PATCH_0x91.md, e46_baud91.bin}`
(checksum tool confirms both TDI sums still valid on the patched image).

**Toolkit changes:**
- `src/protocol.rs` — `frame_baud(baud, p2)` builder (0x91 frame).
- `src/main.rs` — `baud` subcommand: send 0x91 @9600 (expect silence) →
  settle → `set_baud(38400)` → drain pending ACK (raw_read) → verify with
  IDENT (3×) → on failure fall back to 9600 to distinguish "never switched"
  (stock firmware) from "dead" (power-cycle needed).
- 43 unit tests pass; `cargo check` clean (only pre-existing warnings).

**Bench test when the ECU is back:**
1. flash `e46_baud91.bin` (full image; pristine backup `e46_orig.bin`)
2. `dde3-toolkit ident --port /dev/ttyUSB1` (9600 sanity)
3. `dde3-toolkit baud --port /dev/ttyUSB1` → expect IDENT reply @38.4k
4. power cycle → `ident` again at 9600 (revert works)

