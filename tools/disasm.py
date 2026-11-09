#!/usr/bin/env python3
"""Disassemble jobs of a DDE3 .prg file (EDIC15/EDIABAS bytecode).

Usage:
    python3 tools/disasm.py <prg> [job ...] [--limit N]
"""
import struct
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prg_vm import Prg  # noqa: E402

MODES = {0: 'none', 1: 'regs', 2: 'regab', 3: 'regi', 4: 'regl', 5: 'imm8',
         6: 'imm16', 7: 'imm32', 8: 'immstr', 9: 'idximm', 10: 'idxreg',
         11: 'idxregimm', 12: 'idximmlenimm', 13: 'idximmlenreg',
         14: 'idxreglenimm', 15: 'idxreglenreg'}
OPCODES = {0x00: 'move', 0x01: 'clear', 0x02: 'comp', 0x03: 'subb', 0x04: 'adds',
           0x05: 'mult', 0x06: 'divs', 0x07: 'and', 0x08: 'or', 0x09: 'xor',
           0x0A: 'not', 0x0B: 'jump', 0x0C: 'jtsr', 0x0D: 'ret', 0x0E: 'jc',
           0x0F: 'jae', 0x10: 'jz', 0x11: 'jnz', 0x12: 'jv', 0x13: 'jnv',
           0x14: 'jmi', 0x15: 'jpl', 0x16: 'clrc', 0x17: 'setc', 0x18: 'asr',
           0x19: 'lsl', 0x1A: 'lsr', 0x1B: 'asl', 0x1C: 'nop', 0x1D: 'eoj',
           0x1E: 'push', 0x1F: 'pop', 0x20: 'scmp', 0x21: 'scat', 0x22: 'scut',
           0x23: 'slen', 0x24: 'spaste', 0x25: 'serase', 0x26: 'xconnect',
           0x27: 'xhangup', 0x28: 'xsetpar', 0x29: 'xawlen', 0x2A: 'xsend',
           0x2B: 'xsendf', 0x2C: 'xrequf', 0x2D: 'xstopf', 0x2E: 'xkeyb',
           0x2F: 'xstate', 0x30: 'xboot', 0x31: 'xreset', 0x32: 'xtype',
           0x33: 'xvers', 0x34: 'ergb', 0x35: 'ergw', 0x36: 'ergd',
           0x37: 'ergi', 0x38: 'ergr', 0x39: 'ergs', 0x3A: 'a2flt',
           0x3B: 'fadd', 0x3C: 'fsub', 0x3D: 'fmul', 0x3E: 'fdiv',
           0x3F: 'ergy', 0x40: 'enewset', 0x41: 'etag', 0x42: 'xreps',
           0x43: 'gettmr', 0x44: 'settmr', 0x45: 'sett', 0x46: 'clrt',
           0x47: 'jt', 0x48: 'jnt', 0x49: 'addc', 0x4A: 'subc', 0x4B: 'break',
           0x4C: 'clrv', 0x4D: 'eerr', 0x4E: 'popf', 0x4F: 'pushf',
           0x50: 'atsp', 0x51: 'swap', 0x52: 'setspc', 0x53: 'srevrs',
           0x54: 'stoken', 0x55: 'parb', 0x56: 'parw', 0x57: 'parl',
           0x58: 'pars', 0x59: 'fclose', 0x5A: 'jg', 0x5B: 'jge', 0x5C: 'jl',
           0x5D: 'jle', 0x5E: 'ja', 0x5F: 'jbe', 0x60: 'fopen', 0x61: 'fread',
           0x62: 'freadln', 0x63: 'fseek', 0x64: 'fseekln', 0x65: 'ftell',
           0x66: 'ftellln', 0x67: 'a2fix', 0x68: 'fix2flt', 0x69: 'parr',
           0x6A: 'test', 0x6B: 'wait', 0x6C: 'date', 0x6D: 'time',
           0x7B: 'tabset', 0x7C: 'tabseek', 0x7D: 'tabget'}


def disasm(b, addr, limit=400):
    off = addr
    out = []
    while len(out) < limit:
        if off >= len(b):
            break
        oc = b[off]
        am = b[off + 1] if off + 1 < len(b) else 0
        m0 = (am >> 4) & 0xF
        m1 = am & 0xF
        txt = f'{off:08x}  {OPCODES.get(oc, f"?{oc:02x}"):8s}'
        o2 = off + 2
        a0 = arg(b, o2, m0)
        o2b = a0[0]
        a1 = arg(b, o2b, m1)
        o2c = a1[0]
        line = txt + a0[1] + (',' + a1[1] if a1[1] else '')
        out.append(line)
        off = o2c
        if oc == 0x1D:  # eoj
            break
    return out


def arg(b, off, mode):
    n = len(b)
    if off + 4 > n:
        return off + 2, '?'
    if mode == 0:
        return off, ''
    if mode in (1, 2, 3, 4):
        return off + 1, f'r[{b[off]}]'
    if mode == 5:
        return off + 1, f'#{b[off]:#04x}'
    if mode == 6:
        return off + 2, f'#{struct.unpack_from("<H", b, off)[0]:#06x}'
    if mode == 7:
        return off + 4, f'#{struct.unpack_from("<I", b, off)[0]:#08x}'
    if mode == 8:
        ln = struct.unpack_from('<h', b, off)[0]
        s = b[off + 2:off + 2 + max(ln, 0)]
        return off + 2 + ln, repr(s)
    if mode == 9:
        a = struct.unpack_from('<H', b, off)[0]
        return off + 4, f'[{a:#06x}]'
    if mode == 10:
        return off + 3, f'[r[{b[off+1]}]+?]'
    if mode == 11:
        return off + 3, f'[r[{b[off+1]}]+?]'
    if mode in (12, 13):
        return off + 5, '?idxlen'
    if mode in (14, 15):
        return off + 3, f'[r[{b[off+1]}] len=?]'
    return off + 2, f'?{MODES.get(mode)}'


def main():
    prg = Prg(sys.argv[1])
    limit = 400
    jobs = []
    for a in sys.argv[2:]:
        if a == '--limit':
            continue
        jobs.append(a)
    if '--limit' in sys.argv:
        limit = int(sys.argv[sys.argv.index('--limit') + 1])
    if not jobs:
        print('no jobs given; use: disasm.py <prg> JOB [JOB ...] [--limit N]')
        return
    for job in jobs:
        addr = prg.jobs[job]
        print(f'=== {job} @ {addr:#06x} ===')
        for line in disasm(prg.buf, addr, limit):
            print(line)
        print()


if __name__ == '__main__':
    main()
