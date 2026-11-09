#!/usr/bin/env python3
"""Passive DDE3 K-line monitor (ECU-side tap).

Runs on the UART-LPT adapter (/dev/ttyUSB0), which sees exactly what the
ECU receives from the FT232R sender (/dev/ttyUSB1). It cannot drive the
bus, so this tool only *observes*: it frames the traffic (payload +
trailing XOR, silence-delimited), verifies the XOR checksum, and names the
DDE3 service. Use it while running the toolkit to watch the ECU-side view
of every transaction.

Usage:  python3 monitor.py [--port /dev/ttyUSB0] [--baud 9600] [--quiet]
"""

import argparse
import sys
import time

try:
    import serial
except ImportError:
    sys.exit("pyserial required: pip install pyserial")

# Keyed by (sid=payload[2], sub=payload[3] if present). Payload is
# `12 <len> <sid> [sub] ...` — e.g. IDENT is `12 04 00` (sid 0x00, no sub).
SERVICES = {
    (0x00, None): "IDENT",
    (0x0D, None): "UPROG_EIN",
    (0x0A, None): "STATUS_CODIER_CHECKSUMME",
    (0x9F, None): "DIAGNOSE_ENDE",
    (0x6D, None): "STATUS_SYNC_MODE",
    (0x90, 0x42): "SEED_KEY (12 08 90 42 4D 57 05 Z)",
    (0x06, None): "FLASH_LESEN",
    (0x07, 0x06): "FLASH_LOESCHEN (erase)",
    (0x07, 0x02): "FLASH_SCHREIBEN (write)",
    (0x07, 0x0F): "FLASH_SCHREIBEN_ENDE",
    (0x07, 0x0E): "DATENBEREICH_LOESCHEN_0E (fault clear)",
    (0x0B, 0x10): "MW_SELECT_LESEN (live data)",
    (0x0B, 0x03): "MW status (live data)",
    (0x90, 0x56): "SCHALTE_PROTOKOLL_DS2_NACH_K2",
    (0x04, 0x01): "FS_LESEN",
}


def xor(data: bytes) -> int:
    v = 0
    for b in data:
        v ^= b
    return v


def extract(buf: bytes):
    """Try to pull one complete frame off the front of `buf`.

    Returns (payload, rest) when the prefix up to some k forms a valid
    frame (trailing XOR at k), else (None, buf). This splits back-to-back
    frames that arrived in a single serial read.
    """
    if len(buf) < 3 or buf[0] != 0x12:
        return None, buf
    for k in range(2, len(buf)):
        if xor(buf[:k]) == buf[k]:
            return buf[:k], buf[k + 1:]
    return None, buf


def name(payload: bytes) -> str:
    if len(payload) < 3 or payload[0] != 0x12:
        return "?"
    key = (payload[2], payload[3] if len(payload) > 3 else None)
    if key in SERVICES:
        return SERVICES[key]
    if key[0] in SERVICES:
        return SERVICES[key[0], None]
    return f"service 0x{payload[2]:02X}"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--baud", type=int, default=9600)
    ap.add_argument("--quiet", action="store_true", help="no header")
    args = ap.parse_args()

    if not args.quiet:
        print(f"monitor: {args.port} @ {args.baud} 8E1 (passive ECU-side tap)")
        print("watching for DDE3 frames <payload> <XOR> ... (Ctrl-C to stop)\n")

    ser = serial.Serial(args.port, args.baud, timeout=0.2,
                        parity=serial.PARITY_EVEN)
    t0 = time.time()
    buf = b""
    count = 0
    try:
        while True:
            chunk = ser.read(256)
            if chunk:
                buf += chunk
            while True:
                payload, rest = extract(buf)
                if payload is None:
                    if len(buf) > 128:  # resync: drop leading garbage byte
                        buf = buf[1:]
                    break
                now = time.time() - t0
                print(f"[{now:9.3f}] OK  {payload.hex(' ')}  {name(payload)}")
                count += 1
                buf = rest
    except KeyboardInterrupt:
        pass
    finally:
        ser.close()
        print(f"\n{count} frames seen")


if __name__ == "__main__":
    main()
