#!/usr/bin/env python3
"""
DDE3 .prg (EDIABAS job) virtual machine.

Decodes the 10DDE30.prg bytecode and executes individual job functions with
simulated ECU responses, capturing the exact DS2 request frames each function
builds. Used to derive the protocol constants for the dde3-toolkit Rust port.

File format (verified):
  - header plaintext: 0..0xA0
  - body: 0xA0..end XOR 0xF7
  - job/function table @ i32(hdr[0x88]); count (plain) at that offset;
    entries 0x44 bytes: 0x40 name + u32 code address
  - code: [opcode][modebyte (m0<<4|m1)][operands...]
  - register byte ids: B0-B3=0x00-0x03 (bytes of L0), I0/I1=0x10/0x11 (words of
    L0), L0-L2=0x18-0x1A, S0-S7=0x1C-0x23, F0-F3=0x24-0x27
"""
import struct, sys
from collections import deque

class Prg:
    def __init__(self, path):
        raw = open(path, 'rb').read()
        body = bytes(b ^ 0xF7 for b in raw[0xA0:])
        self.buf = raw[:0xA0] + body
        self.jobs = {}
        off = struct.unpack_from('<i', self.buf, 0x88)[0]
        n = struct.unpack_from('<i', raw, off)[0]
        for i in range(n):
            jo = off + 4 + i*0x44
            name = self.buf[jo:jo+0x40].split(b'\0')[0].decode('latin1')
            self.jobs[name] = self.buf[jo+0x40:jo+0x44] and struct.unpack_from('<I', self.buf, jo+0x40)[0]

    def insn(self, off):
        oc = self.buf[off]; am = self.buf[off+1]
        m0 = (am >> 4) & 0xF; m1 = am & 0xF
        return off, oc, m0, m1

    def insn_size(self, off, oc, m0, m1):
        # operand sizes (calibrated against the decompiled listing)
        def ctx(o, m):
            if m in (2, 9, 10):
                return 2 if 0x1C <= self.buf[o] <= 0x23 else 1
            return None
        def sz(m, o):
            c = ctx(o, m)
            if c: return c
            if m == 0: return 0
            if m in (1, 3, 4, 5, 10, 14): return 1
            if m in (2, 6): return 2
            if m == 7: return 4
            if m == 8: return 2 + struct.unpack_from('<h', self.buf, o)[0]
            if m in (9, 11, 15): return 3
            if m == 12: return 5
            if m == 13: return 4
            raise ValueError(f"unknown mode {m}")
        return 2 + sz(m0, off+2) + (sz(m1, off+2+sz(m0, off+2)) if m1 else 0)

# ---- register ids ----
def is_B(r): return 0x00 <= r <= 0x03
def is_I(r): return r in (0x10, 0x11)
def is_L(r): return r in (0x18, 0x19, 0x1A)
def is_S(r): return 0x1C <= r <= 0x23
def is_F(r): return 0x24 <= r <= 0x27

class VM:
    def __init__(self, prg):
        self.prg = prg
        self.reset()

    def reset(self):
        self.S = [bytearray() for _ in range(8)]
        self.L = [0, 0, 0]
        self.F = [0.0, 0.0, 0.0, 0.0]
        self.stack = []
        self.carry = self.overflow = self.zf = self.nf = 0
        self.vars = {}
        self.table = None
        self.requests = []
        self.responses = deque()
        self.log = []
        self.params = {}
        self.ecu_state = {}
        self.result = None

    # ---------- register access ----------
    def getL(self, r): return self.L[r - 0x18] & 0xFFFFFFFF
    def setL(self, r, v): self.L[r - 0x18] = v & 0xFFFFFFFF
    def getI(self, r): return (self.L[0] >> (16 if r == 0x11 else 0)) & 0xFFFF
    def setI(self, r, v):
        if r == 0x11: self.L[0] = (self.L[0] & 0xFFFF) | ((v & 0xFFFF) << 16)
        else: self.L[0] = (self.L[0] & 0xFFFF0000) | (v & 0xFFFF)
    def getB(self, r): return (self.L[0] >> (8 * r)) & 0xFF
    def setB(self, r, v):
        sh = 8 * r; self.L[0] = (self.L[0] & ~(0xFF << sh)) & 0xFFFFFFFF | ((v & 0xFF) << sh)
    def getS(self, r): return self.S[r - 0x1C]
    def setS(self, r, b): self.S[r - 0x1C] = b
    def getF(self, r): return self.F[r - 0x24]

    def val(self, r):
        # unified 32-bit view
        if is_L(r) or is_L(r - 0):
            if is_L(r): return self.getL(r)
        if is_I(r): return self.getI(r)
        if is_B(r): return self.getB(r)
        raise ValueError(hex(r))

    def setreg(self, r, v):
        if is_L(r): self.setL(r, v)
        elif is_I(r): self.setI(r, v)
        elif is_B(r): self.setB(r, v)
        elif is_F(r): self.F[r - 0x24] = float(v)
        elif is_S(r): self.setS(r, v if isinstance(v, (bytes, bytearray)) else str(v).encode())
        else: raise ValueError(hex(r))

    # ---------- operand decode (per opcode context) ----------
    def get_operand(self, off, oc, m, o):
        """Return (value, next_off). m in 0..15."""
        b = self.prg.buf
        if m == 0: return None, o
        if m == 1:  # string register
            r = b[o]; return r, o + 1
        if m == 3:  # I register
            r = b[o]; return r, o + 1
        if m == 4:  # L register
            r = b[o]; return r, o + 1
        if m == 5:  # imm8
            return b[o], o + 1
        if m == 6:  # imm16
            return struct.unpack_from('<H', b, o)[0], o + 2
        if m == 7:  # imm32
            return struct.unpack_from('<i', b, o)[0], o + 4
        if m == 8:  # immstr
            ln = struct.unpack_from('<h', b, o)[0]
            return b[o+2:o+2+ln], o + 2 + ln
        if m == 2 or m == 10:  # string-indexed Sx[Ly] or plain register (context)
            first = b[o]
            if 0x1C <= first <= 0x23:  # S-register byte -> idxreg Sx[Ly]
                return (first, b[o+1]), o + 2
            return first, o + 1
        if m == 9:  # Sx[imm16]
            sr = b[o]
            if 0x1C <= sr <= 0x23:
                idx = struct.unpack_from('<H', b, o+1)[0]
                return (sr, idx), o + 3
            return sr, o + 1  # plain register (rare)
        if m == 15:  # Sx[Ly]Lz (index reg, length reg)
            sr = b[o]; ir = b[o+1]; lr = b[o+2]
            return (sr, ir, lr), o + 3
        raise ValueError(f"mode {m} @ {off:#x}")

    def readval(self, kind):
        """Materialize a value from an operand descriptor."""
        if kind is None: return None
        if isinstance(kind, (tuple,)):
            sr = kind[0]
            if len(kind) == 2:
                idx = self.getL(kind[1])
                return bytes(self.getS(sr)[idx:idx+4])  # 4-byte window (caller trims)
            if len(kind) == 3:
                idx = self.getL(kind[1]); ln = self.getL(kind[2])
                return bytes(self.getS(sr)[idx:idx+ln])
        if isinstance(kind, int) and (is_B(kind) or is_I(kind) or is_L(kind)):
            return kind
        if isinstance(kind, (bytes, bytearray)):
            return bytes(kind)
        return kind

    # ---------- string ops ----------
    def sget4(self, sr, idx):
        s = self.getS(sr)
        out = bytearray(4)
        for i in range(4):
            out[i] = s[idx + i] if idx + i < len(s) else 0
        return bytes(out)

    # ---------- flag helpers ----------
    def cmp_flags(self, a, b):
        a &= 0xFFFFFFFF; b &= 0xFFFFFFFF
        diff = (a - b) & 0xFFFFFFFF
        self.zf = 1 if diff == 0 else 0
        self.nf = (diff >> 31) & 1
        self.carry = 1 if a < b else 0
        sa = (a >> 31) & 1; sb = (b >> 31) & 1
        self.overflow = 1 if (sa != sb and ((diff >> 31) & 1) != sa) else 0

    def arith_flags(self, res, a, b, op):
        res &= 0xFFFFFFFF
        self.zf = 1 if res == 0 else 0
        self.nf = (res >> 31) & 1
        if op == 'adds':
            sa = (a >> 31) & 1; sb = (b >> 31) & 1; sr = (res >> 31) & 1
            self.carry = 1 if (res ^ a) & (res ^ b) & 0x80000000 == 0 and sr and sa == sb else 0
            self.carry = 1 if ((a + b) & 0xFFFFFFFF) != res else 0
            self.overflow = 1 if sa == sb and sr != sa else 0
        elif op == 'subb':
            self.carry = 1 if a < b else 0
            sa = (a >> 31) & 1; sb = (b >> 31) & 1
            self.overflow = 1 if sa != sb and ((res >> 31) & 1) != sa else 0
        else:
            self.carry = self.overflow = 0

    # ---------- execution ----------
    def step(self, pc):
        off, oc, m0, m1 = self.prg.insn(pc)
        o = pc + 2
        v0, o = self.get_operand(pc, oc, m0, o)
        v1 = None
        if m1:
            v1, o = self.get_operand(pc, oc, m1, o)
        nxt = o
        status = self.exec(oc, m0, v0, m1, v1, pc)
        # On 'next', advance to the next instruction. On 'jump', exec() has
        # already set self.pc to the absolute target. On 'stop', we break out
        # in run(). (Previously step() discarded exec's status and returned
        # nxt, so run() re-advanced pc after every jump — the double-advance bug.)
        if status == 'next':
            self.pc = nxt
        return status

    def exec(self, oc, m0, v0, m1, v1, pc):
        P = self
        if oc == 0x00:  # move
            val = self.eval_src(m1, v1)
            self.assign(m0, v0, val)
        elif oc == 0x01:  # clear
            self.assign(m0, v0, 0 if is_L(v0) else (b'' if is_S(v0) else 0))
        elif oc == 0x02:  # comp Lx,Ly / Lx,#imm
            a = self.getL(v0)
            b_ = self.getL(v1) if m1 == 4 else (v1 & 0xFFFFFFFF)
            self.cmp_flags(a, b_)
        elif oc == 0x03:  # subb
            a = self.getL(v0); b_ = self.getL(v1) if m1 == 4 else (v1 & 0xFFFFFFFF)
            res = (a - b_) & 0xFFFFFFFF
            self.arith_flags(res, a, b_, 'subb'); self.setL(v0, res)
        elif oc == 0x04:  # adds
            a = self.getL(v0); b_ = self.getL(v1) if m1 == 4 else (v1 & 0xFFFFFFFF)
            res = (a + b_) & 0xFFFFFFFF
            self.arith_flags(res, a, b_, 'adds'); self.setL(v0, res)
        elif oc == 0x05:  # mult
            a = self.getL(v0); b_ = self.getL(v1)
            res = (a * b_) & 0xFFFFFFFF
            self.arith_flags(res, a, b_, 'mult'); self.setL(v0, res)
        elif oc == 0x06:  # divs
            a = self.getL(v0); b_ = self.getL(v1)
            sa = a >> 31; sb = b_ >> 31
            aa = a if sa == 0 else (~a + 1) & 0xFFFFFFFF
            bb = b_ if sb == 0 else (~b_ + 1) & 0xFFFFFFFF
            q = 0 if bb == 0 else aa // bb
            if sa != sb: q = (~q + 1) & 0xFFFFFFFF
            self.arith_flags(q, a, b_, 'divs'); self.setL(v0, q)
        elif oc == 0x07:  # and
            a = self.getL(v0); b_ = self.getL(v1)
            res = a & b_; self.arith_flags(res, a, b_, 'and'); self.setL(v0, res)
        elif oc == 0x08:  # or
            a = self.getL(v0); b_ = self.getL(v1)
            res = a | b_; self.arith_flags(res, a, b_, 'or'); self.setL(v0, res)
        elif oc == 0x09:  # xor
            a = self.getL(v0); b_ = self.getL(v1)
            res = a ^ b_; self.arith_flags(res, a, b_, 'xor'); self.setL(v0, res)
        elif oc == 0x0B:  # jump
            t = pc + 6 + v0
            self.pc = t
            return 'jump'
        elif oc in (0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
                    0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F):
            cond = self.jcond(oc)
            if cond:
                self.pc = pc + 6 + v0
                return 'jump'
        elif oc in (0x18, 0x1A, 0x1B):  # asr/lsr/asl
            a = self.getL(v0); sh = self.getL(v1) & 0x1F
            if oc == 0x18:
                res = ((a >> 31) & 1) * 0xFFFFFFFF << sh if False else ((a & 0xFFFFFFFF) >> sh) | (0xFFFFFFFF << (32 - sh) if sh else 0)
                res = (a >> sh) | (0xFFFFFFFF << (32 - sh)) if sh else a
            elif oc == 0x1A:
                res = (a << sh) & 0xFFFFFFFF
            else:
                res = ((a << sh) & 0xFFFFFFFF) | ((a >> (32 - sh)) << sh) if sh else a
            self.arith_flags(res, a, 0, 'sh'); self.setL(v0, res)
        elif oc == 0x1E:  # push
            if m0 == 7: self.stack.append(v0)
            else: self.stack.append(self.getL(v0))
        elif oc == 0x1F:  # pop
            v = self.stack.pop() & 0xFFFFFFFF
            self.setL(v0, v)
        elif oc == 0x21:  # scat S1,S2 (concat: S1 = S1+S2?)
            # EDIABAS scat: source appended to dest? we use S1 = S1 + S2
            a = bytes(self.getS(v0)); c = bytes(self.getS(v1))
            self.setS(v0, a + c)
        elif oc == 0x22:  # scut S1,#n -> truncate to n
            s = self.getS(v0); self.setS(v0, s[:v1])
        elif oc == 0x23:  # slen L0,S5
            self.setL(v0, len(self.getS(v1)))
        elif oc == 0x24:  # spaste Sx[imm],S1  (insert at offset, extending)
            (sr, idx) = v0
            s = bytearray(self.getS(sr)); ins = bytes(self.getS(v1))
            s[idx:idx] = ins
            self.setS(sr, s)
        elif oc == 0x25:  # serase Sx[imm],L0 (erase L0 bytes at idx)
            (sr, idx) = v0
            s = bytearray(self.getS(sr)); n = self.getL(v1)
            del s[idx:idx+n]
            self.setS(sr, s)
        elif oc == 0x23:
            pass
        elif oc in (0x26, 0x2D, 0x31, 0x40):  # xconnect/xstopf/xreset/enewset
            self.log.append((pc, hex(oc)))
        elif oc == 0x28:  # xsetpar
            self.log.append((pc, 'xsetpar', bytes(v0) if m0 == 8 else self.getS(v0).hex()))
        elif oc == 0x29:  # xawlen
            self.log.append((pc, 'xawlen', self.getS(v0).hex()))
        elif oc == 0x2A:  # xsend DST,SRC  (or DST,immstr)
            dst = v0
            src = bytes(v1) if m1 == 8 else bytes(self.getS(v1))
            self.xsend(pc, dst, src)
        elif oc == 0x37:  # ergi NAME,I0
            name = self._varname(v0); r = v1
            val = self.getI(r) if is_I(r) else self.getL(r)
            self.vars[name] = val
        elif oc == 0x38:  # ergr NAME,F
            self.vars[self._varname(v0)] = self.getF(v1)
        elif oc == 0x39:  # ergs NAME,S
            self.vars[self._varname(v0)] = bytes(self.getS(v1))
        elif oc == 0x3F:  # ergy NAME,S
            self.vars[self._varname(v0)] = bytes(self.getS(v1))
        elif oc == 0x3A:  # a2flt F1,S1
            t = bytes(self.getS(v1)).decode().split('\x00')[0].strip()
            self.F[v0 - 0x24] = float(t or '0')
        elif oc == 0x3D:  # fmul F2,F1
            self.F[v0 - 0x24] = self.F[v0 - 0x24] * self.F[v1 - 0x24]
        elif oc == 0x41:  # etag OFFSET,NAME
            self.log.append((pc, 'etag', v0, bytes(v1).decode()))
        elif oc == 0x42:  # xreps N
            self.reps = v0
        elif oc == 0x50:  # atsp Lr,#n : Lr = stack top (peek); param base = n
            if self.stack:
                self.setL(v0, self.stack[-1] & 0xFFFFFFFF)
            else:
                self.setL(v0, 0)
        elif oc == 0x56:  # parw Ir,#n
            self.setI(v0, self.param(v1) & 0xFFFF)
        elif oc == 0x57:  # parl Lr,#n
            self.setL(v0, self.param(v1))
        elif oc == 0x58:  # pars Sr,#n
            self.setS(v0, self.param(v1))
        elif oc == 0x7F:  # pary Sr (next string param)
            self.setS(v0, self.pary_next())
        elif oc == 0x67:  # a2fix L0,S
            self.setL(v0, self.a2fix(bytes(self.getS(v1))))
        elif oc == 0x68:  # fix2flt F,L
            self.F[v0 - 0x24] = float(self.getL(v1))
        elif oc == 0x6B:  # wait I0
            self.log.append((pc, 'wait', self.getI(v0)))
        elif oc == 0x6D:  # time S
            self.log.append((pc, 'time', bytes(self.getS(v0)).decode()))
        elif oc == 0x74:  # xprog I0
            self.log.append((pc, 'xprog', self.getI(v0)))
        elif oc == 0x79:  # fix2hex S,B / S,L
            if m1 == 2:
                self.setS(v0, f"0x{self.getB(v1):02X}".encode())
            else:
                self.setS(v0, f"0x{self.getL(v1):08X}".encode())
        elif oc == 0x7A:  # fix2dez S,L
            self.setS(v0, str(self.getL(v1)).encode())
        elif oc == 0x7B:  # tabset NAME
            # Strip the trailing NUL from the immstr table name (its length
            # includes the NUL), else the key won't match the table.
            self.table = bytes(v0).split(b'\x00')[0].decode()
        elif oc == 0x7C:  # tabseek COL,S
            self.zf = self.tabseek(bytes(v0).decode(), bytes(self.getS(v1)))
        elif oc == 0x7D:  # tabget S,COL
            self.setS(v0, self.tabget(bytes(v1).decode()))
        elif oc == 0x7E:  # strcat S,S / S,immstr
            a = bytes(self.getS(v0))
            c = bytes(v1) if m1 == 8 else bytes(self.getS(v1))
            self.setS(v0, a + c)
        elif oc == 0x8F:  # strcmp S,immstr
            # Compare with trailing NULs stripped; zf=1 iff equal (do NOT reuse
            # cmp_flags — its zero-flag was inverted for this purpose).
            a = bytes(self.getS(v0)).decode().rstrip('\x00')
            c = bytes(v1).decode().rstrip('\x00')
            self.zf = 1 if a == c else 0
            self.nf = self.carry = self.overflow = 0
        elif oc == 0x1C:  # nop
            pass
        elif oc == 0x1D:  # eoj
            self.result = 'eoj'
            return 'stop'
        else:
            raise NotImplementedError(f"oc={oc:#04x} @ {pc:#x}")
        return 'next'

    def jcond(self, oc):
        if oc == 0x0E: return not self.carry            # jc: jump if carry clear? (unsigned >)
        if oc == 0x0F: return self.carry               # jae: unsigned >=
        if oc == 0x10: return self.zf                  # jz
        if oc == 0x11: return not self.zf              # jnz
        if oc == 0x12: return self.overflow            # jv
        if oc == 0x13: return not self.overflow        # jnv
        if oc == 0x14: return self.nf                  # jmi
        if oc == 0x15: return not self.nf              # jpl
        if oc == 0x5A: return (not self.zf and self.nf == self.overflow)  # jg
        if oc == 0x5B: return (self.zf or (not self.zf and self.nf == self.overflow))  # jge
        if oc == 0x5C: return (not self.zf and self.nf != self.overflow)  # jl
        if oc == 0x5D: return (self.zf or (not self.zf and self.nf != self.overflow))  # jle
        if oc == 0x5E: return not self.carry           # ja (unsigned >)
        if oc == 0x5F: return self.carry               # jbe (unsigned <=)
        raise ValueError(hex(oc))

    def eval_src(self, m, v):
        """Value of a source operand (m = operand mode, v = decoded raw value)."""
        if m == 0: return None
        if m in (5, 6):  # immediates
            return v
        if m == 7:
            # A 32-bit mode-7 operand can ALSO encode a register id (I/L/S/F).
            # B-ids are never 4-byte registers (e.g. #$2.L == imm 2, not B2).
            if is_I(v): return self.getI(v)
            if is_L(v): return self.getL(v)
            if is_S(v): return bytes(self.getS(v))
            if is_F(v): return self.getF(v)
            return v
        if m == 8: return bytes(v)
        if isinstance(v, tuple):  # string-indexed source
            if len(v) == 3:
                (sr, ir, lr) = v
                idx = self.getL(ir); ln = self.getL(lr)
                return bytes(self.getS(sr)[idx:idx+ln])
            (sr, rest) = v[0], v[1]
            idx = self.getL(rest) if isinstance(rest, int) and 0x18 <= rest <= 0x1A else rest
            return self.sget4(sr, idx)
        # plain register
        if is_B(v): return self.getB(v)
        if is_I(v): return self.getI(v)
        if is_L(v): return self.getL(v)
        if is_S(v): return bytes(self.getS(v))
        if is_F(v): return self.getF(v)
        return v

    def assign(self, m, reg, val):
        if isinstance(reg, tuple):  # string index target Sx[...]
            sr = reg[0]
            if len(reg) == 2 and isinstance(reg[1], int) and 0x18 <= reg[1] <= 0x1A:
                idx = self.getL(reg[1])
            else:
                idx = reg[1]
            s = bytearray(self.getS(sr))
            data = bytes(val) if isinstance(val, (bytes, bytearray)) else bytes([self._to_byte(val)])
            s[idx:idx+len(data)] = data
            self.setS(sr, s)
            return
        if m == 8: return
        if is_S(reg):
            if isinstance(val, (bytes, bytearray)): self.setS(reg, bytes(val))
            elif isinstance(val, int): self.setS(reg, val.to_bytes(4, 'little', signed=True))
            else: self.setS(reg, str(val).encode())
        elif is_L(reg): self.setL(reg, self._to_int(val))
        elif is_I(reg): self.setI(reg, self._to_int(val) & 0xFFFF)
        elif is_B(reg): self.setB(reg, self._to_byte(val))
        elif is_F(reg): self.F[reg - 0x24] = float(val)

    def param(self, n):
        v = self.params.get(n)
        if v is None:
            raise KeyError(f"param {n} not provided")
        if isinstance(v, int): return v & 0xFFFFFFFF
        return v

    def pary_next(self):
        # sequential string params, tracked via self._pary_idx
        if not hasattr(self, '_pary_idx'): self._pary_idx = 0
        self._pary_idx += 1
        return self.params.get(self._pary_idx, b'')

    def _varname(self, b):
        # Variable names are NUL-terminated immediates; strip the NUL so keys
        # land in vm.vars without a trailing \x00.
        return bytes(b).split(b'\x00')[0].decode('latin1')

    @staticmethod
    def _to_int(val):
        if isinstance(val, int):
            return val & 0xFFFFFFFF
        if isinstance(val, float):
            return int(val) & 0xFFFFFFFF
        return int.from_bytes(val[:4], 'little')

    @staticmethod
    def _to_byte(val):
        if isinstance(val, (int, float)):
            return int(val) & 0xFF
        return val[0]

    def a2fix(self, s):
        t = s.decode().split('\x00')[0].strip()
        if not t: return 0
        if t.lower().startswith('0x'): return int(t, 16)
        try: return int(t, 16)
        except ValueError: return int(t, 10)

    # ---------- tables ----------
    TABLES = {
        'JOBRESULT': [
            ('0xA0', 'OKAY'),
            ('0xA1', 'BUSY'),
            ('0xA2', 'ERROR_SG_REJECTED'),
            ('0xB0', 'ERROR_SG_PARAMETER'),
            ('0xB1', 'ERROR_SG_FUNCTION'),
            ('0xB2', 'ERROR_SG_NUMBER'),
            ('0xFF', 'ERROR_SG_NACK'),
            ('0x00', 'ERROR_SG_UNBEKANNTES_STATUSBYTE'),
        ],
    }

    def tabseek(self, col, key):
        # Case-insensitive table lookup: the .prg names the table "JobResult"
        # but the embedded table key is "JOBRESULT".
        key = key.decode().rstrip('\x00')
        tname = (self.table or '').lower()
        rows = None
        for name, r in self.TABLES.items():
            if name.lower() == tname:
                rows = r
                break
        if rows is None:
            self._trow = None
            return 0
        for row in rows:
            if row[0] == key:
                self._trow = row
                return 1
        self._trow = None
        return 0

    def tabget(self, col):
        row = getattr(self, '_trow', None)
        if row is None: return b''
        col = col.rstrip('\x00')
        # col names: 0=SB, 1=STATUS_TEXT
        return row[1 if col == 'STATUS_TEXT' else 0].encode()

    # ---------- IO ----------
    def xsend(self, pc, dst, src):
        self.requests.append((pc, bytes(src)))
        if self.responses:
            resp = self.responses.popleft()
        else:
            resp = b''
        self.log.append((pc, 'xsend', src.hex(), '->', resp.hex() if resp else '<none>'))
        self.setS(dst, resp)

    # ---------- driver ----------
    def run(self, name, max_steps=200000):
        addr = self.prg.jobs[name]
        self.pc = addr
        self._pary_idx = 0
        steps = 0
        while True:
            steps += 1
            if steps > max_steps:
                raise RuntimeError(f"step limit in {name}")
            status = self.step(self.pc)
            if status == 'stop':
                break
            # 'jump' -> self.pc already set by exec(); 'next' -> set by step().
        return self

def dump_requests(vm, name):
    print(f"===== {name} =====")
    for pc, req in vm.requests:
        print(f"  {pc:#08x}  {bytes(req).hex(' ')}")
    for k, v in vm.vars.items():
        if isinstance(v, bytes):
            print(f"  var {k} = {v!r}")
        else:
            print(f"  var {k} = {v}")

if __name__ == '__main__':
    prg = Prg(sys.argv[1])
    print(f"{len(prg.jobs)} jobs:", ", ".join(prg.jobs))
