#!/usr/bin/env python3
"""One-off capture: IDENT + UPROG_EIN + one FLASH_LESEN against the REAL ECU.

Drives the K-line from a full-duplex adapter (the same port the toolkit's
`respond` fake ECU sits on), sends exactly three frames, and logs everything
the ECU answers with:

    1. IDENT        12 04 00 16
    2. UPROG_EIN    12 04 0D 1B
    3. FLASH_LESEN  12 09 06 00 00 00 00 2E 33   (addr=0x000000, count=46 —
                                                  the AIF / hardware-ID read)

Line convention is the same as `respond` / `capture` (timestamp = seconds
since start):

    [   12.345] TX   12 04 00 16                 IDENT
    [   12.400] ECHO 12 04 00 16                 (our own TX, bus echo)
    [   12.460] RX   12 2E A0 37 37 38 36 …      (ECU reply)
    [   12.500] RAW  CF CF

The adapter's RX sees our own transmissions (bus echo). Every byte is
framed with the real convention — `12 <LEN> … <XOR>`, LEN = total wire
length INCLUDING the trailing XOR (docs/wire-protocol.md) — and a frame
whose payload equals the one just sent is logged as ECHO (the same
skip rule as protocol::Kline::do_request); everything else is RX. Bytes
that never form a valid frame are logged verbatim as RAW at the end of
each window.

Usage:
    python3 tools/capture_flash_lesen.py --port /dev/ttyUSB0
    python3 tools/capture_flash_lesen.py --port /dev/ttyUSB0 -o capture.log
    python3 tools/capture_flash_lesen.py --port /dev/ttyUSB0 --addr 0 --count 128
"""

import argparse
import sys
import time

try:
    import serial
except ImportError:
    sys.exit("pyserial required: pip install pyserial")


def xor(data: bytes) -> int:
    v = 0
    for b in data:
        v ^= b
    return v


def build_frame(payload: bytes) -> bytes:
    return payload + bytes([xor(payload)])


def frame_flash_lesen(addr: int, count: int) -> bytes:
    # 12 09 06 00 <A_hi A_mid A_lo> <count>  (+ trailing XOR)
    return build_frame(bytes([0x12, 0x09, 0x06, 0x00,
                              (addr >> 16) & 0xFF, (addr >> 8) & 0xFF, addr & 0xFF,
                              count & 0xFF]))


class Framer:
    """`12 <LEN> … <XOR>` — LEN = total wire bytes incl. the trailing XOR.

    A frame is emitted only when all LEN bytes are present AND the trailing
    XOR validates (same rule as protocol::FrameParser).
    """

    def __init__(self):
        self.buf = bytearray()

    def feed(self, data: bytes):
        self.buf.extend(data)
        if len(self.buf) > 512:  # resync guard: keep the tail
            del self.buf[:len(self.buf) - 256]

    def take(self):
        """Return (payload, complete)."""
        while self.buf:
            if self.buf[0] != 0x12:
                del self.buf[0]  # leading noise
                continue
            if len(self.buf) < 2:
                return None, False
            ln = self.buf[1]
            if not (4 <= ln <= 255) or len(self.buf) < ln:
                return None, False  # implausible LEN or mid-frame
            k = ln - 1  # index of the trailing XOR
            if xor(bytes(self.buf[:k])) == self.buf[k]:
                payload = bytes(self.buf[:k])
                del self.buf[:ln]
                return payload, True
            del self.buf[0]  # XOR mismatch: false anchor, resync
        return None, False


def name(payload: bytes) -> str:
    if len(payload) < 3 or payload[0] != 0x12:
        return "?"
    sid = payload[2]
    if len(payload) == 4 and sid in (0x00, 0x0D):
        return "IDENT" if sid == 0x00 else "UPROG_EIN"
    if sid == 0x06:
        return "FLASH_LESEN"
    if sid == 0x0B:
        return "MW live"
    if payload[2] in (0xA0, 0xA1, 0xA2, 0xB0, 0xB1, 0xB2, 0xFF, 0x00):
        return f"reply (status 0x{payload[2]:02X})"
    return f"service 0x{sid:02X}"


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", required=True, help="serial port (e.g. /dev/ttyUSB0)")
    ap.add_argument("--baud", type=int, default=9600)
    ap.add_argument("--addr", type=lambda s: int(s, 0), default=0,
                    help="FLASH_LESEN address (default 0)")
    ap.add_argument("--count", type=lambda s: int(s, 0), default=46,
                    help="FLASH_LESEN count byte (default 46 = AIF read)")
    ap.add_argument("--timeout", type=int, default=1500,
                    help="ms capture window after each command (default 1500)")
    ap.add_argument("-o", "--out", default=None, help="mirror the log to this file")
    args = ap.parse_args()

    ser = serial.Serial(args.port, args.baud, bytesize=serial.EIGHTBITS,
                        parity=serial.PARITY_EVEN, stopbits=serial.STOPBITS_ONE,
                        timeout=0.05)
    ser.reset_input_buffer()

    t0 = time.time()
    out = open(args.out, "w") if args.out else None

    def emit(line: str):
        print(line, flush=True)
        if out:
            out.write(line + "\n")
            out.flush()

    emit(f"# capture_flash_lesen — real ECU, port={args.port} baud={args.baud} 8E1")
    emit(f"# commands: IDENT, UPROG_EIN, FLASH_LESEN(addr={args.addr:#x}, count={args.count})")

    commands = [
        ("IDENT", build_frame(bytes([0x12, 0x04, 0x00]))),
        ("UPROG_EIN", build_frame(bytes([0x12, 0x04, 0x0D]))),
        (f"FLASH_LESEN(addr={args.addr:#06x},count={args.count})",
         frame_flash_lesen(args.addr, args.count)),
    ]

    framer = Framer()
    tx_pacing = 0.005  # 5 ms/byte — well above the 1.04 ms/byte line time @9600

    for label, wire in commands:
        # --- transmit, paced byte by byte (same convention as Kline::write_frame)
        for b in wire:
            ser.write(bytes([b]))
            ser.flush()
            time.sleep(tx_pacing)
        emit(f"[{time.time() - t0:9.3f}] TX   {wire.hex(' ')}   {label}")

        # --- capture window: skip our echo, log everything the ECU sends ---
        sent_payload = wire[:-1]  # echo = the payload we just put on the wire
        deadline = time.time() + args.timeout / 1000.0
        while time.time() < deadline:
            chunk = ser.read(256)
            if chunk:
                framer.feed(chunk)
                while True:
                    payload, _ = framer.take()
                    if payload is None:
                        break
                    if payload == sent_payload:
                        emit(f"[{time.time() - t0:9.3f}] ECHO {build_frame(payload).hex(' ')}")
                    else:
                        emit(f"[{time.time() - t0:9.3f}] RX   {build_frame(payload).hex(' ')}   "
                             f"{name(payload)}")
        if framer.buf:
            emit(f"[{time.time() - t0:9.3f}] RAW  {bytes(framer.buf).hex(' ')}")
            framer.buf.clear()
        emit(f"[{time.time() - t0:9.3f}] ---- end of {label} window ----")
        time.sleep(0.2)  # settle between commands

    emit(f"[{time.time() - t0:9.3f}] done")
    ser.close()
    if out:
        out.close()
        print(f"log written to {args.out}")


if __name__ == "__main__":
    main()
