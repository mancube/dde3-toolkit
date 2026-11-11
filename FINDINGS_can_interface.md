# CAN vs K-line: can dde3-toolkit talk to the DDE3 ECU over CAN?

Question answered: *"How could dde3-toolkit communicate with the DDE3 ECU
over CAN instead of K-line?"*

Companion to `FINDINGS_k2_protocol.md` (channel map),
`FINDINGS_baud_service_protocol.md` (DS2 framing) and
`FINDINGS_faster_logger.md` (the 38.4 k K2/PEC path).

---

## 1. Verdict

**The DDE3 ECU has no diagnostic interface on CAN.** K-line (DS2 at 9600,
plus the break-entered K2/PEC high-baud world) is its *only* diagnostic
channel. That is a firmware/hardware fact of the EDC15M, not a limitation
of the toolkit.

What CAN **can** do for the toolkit: **passively sniff the ECU's own
vehicle-bus transmit messages** (DME1/DME2/…) and log/decode them as a
K-line-free live-data source. It cannot replace K-line for any
diagnostic service (flash, DTC, seed/key, live MW, actuator tests, coding).

## 2. Evidence

### 2.1 ECU firmware — the CAN port is a vehicle bus, not a diag port

Source: `~/Desktop/edc15m_out/` (disasm `research/out/disasm.txt`,
`research/routines_part1.md`, A2L `EDC15M_7789659.a2l`).

- The C166's 82C527-compatible CAN module lives at `E800`
  (init `0x17004`, RAM self-test `0x1708A`, message-object setup `0x1613A`,
  controller task `0x1605C`/run `0x161B8`, Rx task `0x17E18`).
- CAN is gated by calibration: enable byte at cal `0x3360`
  (`CAN_CTRL_PARAMS_3360`, enabled in the shipped A2L), **9.0 V battery
  gate** (442 counts × 0.020372 V/count), bus-off re-init, hold-off timers
  — textbook vehicle-bus supervision, faults ORT `0x2C` (idx 122/123).
- The full message-object set (18-byte configs at cal `0x3368–0x3464`,
  named in the A2L):

  | Dir | ID | Role (DLC) | A2L name |
  |---|---|---|---|
  | Tx | `0x316` | DME1 engine data (6) | `CAN_TX_0x316_DME1_337A` |
  | Tx | `0x329` | DME2 engine data (6) | `CAN_TX_0x329_DME2_338C` |
  | Tx | `0x235` | vehicle (4) | `CAN_TX_0x235_339E` |
  | Tx | `0x545` | DME3 (4) | `CAN_TX_0x545_3440` |
  | Tx | `0x101`/`0x501`/`0x701` | VP44 pump (PSG) service (8) | `PSG_CAN_TX_*` |
  | Rx | `0x153` | ASC1/DSC (5) | `CAN_RX_0x153_ASC1_33B0` |
  | Rx | `0x43F` | vehicle (5) | `CAN_RX_0x43F_3368` |
  | Rx | `0x613`/`0x615` | Kombi/instr. cluster (8) | `CAN_RX_0x613_*` / `CAN_RX_0x615_*` |
  | Rx | `0x113`/`0x513`/`0x713` | VP44 pump (PSG) (8) | `PSG_CAN_RX_*` |

- **No diagnostic receive object exists** — no tester diagnostic ID of any
  kind (nothing in the `0x758`/`0x750`/`0x7E8`/`0x644` family).
- The DS2 command dispatcher (`0x29128`; cmd map `00 02 04 05 0A 0B 0C 0D
  0E 0F 40/41/42 6C/6D 90 9E/9F`, task `0x290D4`) is fed **only from the
  serial line** (K-line via the S0/PEC serial unit). No KWP2000/UDS
  handler is present anywhere in the dumped image.
- The ECU's own fault catalog treats its CAN as a vehicle node: ORT `0x45`
  "Steuergerät DDE (CAN-Controller)", ORT `0x72` "CAN-Bus", ORT `0x2D`
  (pump comms) — see `research/diag_jobs.md` DTC location table.
- Bit timing (for a future sniffer): init writes CTL0=`0x41`, CTL1=`0x07`,
  BRP(`E806`)=`0xFF`, BTR(`E807`)=`0xE0`. **Not fully decoded** — the
  Infineon C166 82C527 register reference was not obtainable, so the TQ
  math is unconfirmed. E46 M47 PT-CAN is standardly **500 kbit/s**; verify
  empirically with a sniffer before relying on it.

### 2.2 Bench diagnostic programs — they never command the ECU over CAN

Source: `decompiled.txt` (DDE30DS0.prg), `decompiled_winkfp.txt`
(10DDE30.prg), `tools/prg_vm.py` runs.

- All diagnostic `xsend` traffic in both `.prg` files is K-line DS2.
- `SET_EDIC_BAUDRATE` (10DDE30.prg @ `0x10B63`) contains **no `xsend` at
  all** — only tester-side `xsetpar` line reconfig of the bench's EDIC
  (engine-bus) channel. Nothing on the ECU changes
  (`FINDINGS_k2_protocol.md` §2.3).
- The only CAN use in `DDE30DS0.prg` is the "Betriebswerte" jobs, e.g.
  `STATUS_TIMEOUT_CAN` (@ `0x57604`): table-driven read of the ECU's
  *transmitted* CAN telegrams (`BetriebswTab`: `TELEGRAM` / `POS_ADR` /
  `LEN_ADR` / `ADR` hex-string ID parse, then per-measurement
  `COMPU_TYPE`/`FACT_A`/`FACT_B` scaling) — e.g. pkm pump status,
  "Schalter Klimakompressor KO (CAN)" / "Schalter Klimabereitschaft AC
  (CAN)" digital inputs. The bench **observes** the vehicle bus as a
  sensor; it does not address the ECU on it.

### 2.3 Channel map (recap, from `FINDINGS_k2_protocol.md` §4)

| # | Channel | Baud | Diagnostic? |
|---|---|---|---|
| 1 | DS2 (K-line) | 9600 8E1, fixed | **Yes** — the only diag channel |
| 2 | K2/PEC (K-line) | 10.4 k / 38.4 k, break or in-band `12 05 90 56` | Test-bench bulk RAM/EEPROM/PEC |
| 3 | EDIC / vehicle CAN | (CAN, ~500 k) | **No** — engine-network bus; .prg only reconfigures the tester side |
| 4 | EWS interface | in-band DS2 `6C/6D` | Immobilizer sync |

## 3. What a CAN mode in dde3-toolkit would be

Not "DS2 over CAN" (impossible, §2) — a **passive CAN sniffer / live
logger** on the vehicle PT-CAN:

- **Bus access:** E46 16-pin OBD-II connector — K-line is pin 7 (current
  setup); CAN H/L are on the OBD connector as well (confirm continuity to
  the ECU's CAN transceiver on the actual car). Bus is active only when
  CAN is enabled in cal (it is) and battery ≥ 9.0 V (firmware gate).
- **Baud:** 500 kbit/s first (E46 standard); see §2.1 open item.
- **Transport:** new `src/can.rs`, independent of `Kline`/`protocol.rs`:
  - Linux SocketCAN (`can0`, or `vcan0` for sim/tests) — `socketcan` crate,
    or PCAN/Kvaser USB via their crates; ELM327-CAN-over-serial is a
    fallback using the existing `serialport` dependency.
- **CLI:** mirrors the string-dispatch subcommands in `main.rs`:

  ```sh
  dde3-toolkit can-log --iface can0 --baud 500000 \
      --ids 0x316,0x329,0x235,0x545 --out can.csv --dur 60
  ```

  CSV shape consistent with the Logger tab: `ts, id, dlc, payload`.
- **GUI:** a CAN tab (or section of Logger) with a live table of the four
  DME Tx IDs — last frame, frame rate, error counters — toggled like the
  existing `--auto-live` pattern.
- **Decoding caveat:** the A2L carries the message *objects* but **no
  per-signal bit layout** for DME1/DME2. Signal decode needs an external
  E46 PT-CAN DBC or manual reverse-engineering; raw frame logging works
  without either.
- **Bonus:** run K-line MW logger + CAN sniffer in parallel (dual-adapter
  pattern already used by the monitor setup) to cross-check DS2 values
  against the ECU's own bus snapshot.

## 4. What CAN cannot do on this ECU

Flash read/write, flash erase, DTC read/clear, seed/key, live MW
measurement reads, actuator override, coding/checksum services, K2/PEC
bulk access — **none of these services exist in the ECU's CAN stack**
(§2.1: no diagnostic Rx object; §2.2: bench programs never send diag
traffic on CAN).

If the underlying goal is "faster than 9600 K-line", the legitimate fast
path on this ECU is the **break-entered K2/PEC 38.4 k** logger path
(`FINDINGS_faster_logger.md`) — same K-line, different protocol world.

## 5. Verification status (honest list)

| Claim | Status |
|---|---|
| ECU CAN = 14 vehicle-bus objects, no diag Rx object | **Known** — A2L objects + disasm of init/object-setup/tasks |
| DS2 only on the serial line | **Known** — dispatcher/receiver disasm (0x29128, serial S0/PEC) |
| .prg files never send diag traffic on CAN | **Known** — full decompilation of both job sets |
| E46 M47 PT-CAN = 500 kbit/s | **Inferred** — platform standard; firmware bit timing (BRP=0xFF, BTR=0xE0, CTL1=0x07) not fully decoded without the C166 82C527 register reference; confirm with a sniffer |
| OBD-II pin 6/14 = CAN L/H on the specific car | **Unverified** — continuity check needed |
| DME1/DME2 signal layout | **Unknown** — A2L has objects only; needs a DBC or RE |

## 6. References

| Thing | Where |
|---|---|
| CAN init / self-test / objects / tasks (firmware) | `edc15m_out/research/out/disasm.txt` `0x17004`/`0x1708A`/`0x1613A`/`0x1605C`/`0x161B8`/`0x17E18`; `research/routines_part1.md` modules F/G |
| Message object IDs/DLCs | `edc15m_out/EDC15M_7789659.a2l` (`CAN_*`, `PSG_CAN_*` characteristics, cal 0x3368–0x3464) |
| CAN enable/battery gate cal | A2L `CAN_CTRL_PARAMS_3360_*` (cal 0x3360) |
| DS2 command map + "serial only" | `edc15m_out/research/routines_part3.md` §DS2; `diag_jobs.md` §1 |
| DTC locations ORT 0x45 / 0x72 / 0x2C / 0x2D | `edc15m_out/research/diag_jobs.md` DTC table |
| Bench job CAN usage (`STATUS_TIMEOUT_CAN`) | `decompiled.txt` @ `0x57604`; `edc15m_out/research/diag_jobs.md` job 146 |
| `SET_EDIC_BAUDRATE` = tester-side only | `decompiled_winkfp.txt` @ `0x10B63`; `FINDINGS_k2_protocol.md` §2.3 |
| Channel map | `FINDINGS_k2_protocol.md` §4 |
| Toolkit transport/CLI structure for the sketch | `src/protocol.rs` (`Kline`), `src/main.rs` subcommand dispatch |
