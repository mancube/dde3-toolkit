#!/usr/bin/env python3
"""Run the parameter-dependent jobs with plausible parameter values and
capture the xsend frames they build. Appends results to analysis/frames.md
(as a 'Parameterized runs' section) and prints them.

Parameters are guesses based on the decompiled listing's parl/parw/pars/pary
usage; only the frame *structure* is meaningful (data fields reflect the
simulated response, not a real ECU).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prg_vm import Prg, VM  # noqa: E402

PRG = os.environ.get('DDE3_PRG', '/home/ilija/Desktop/logger/10DDE30.prg')
GENERIC = bytes.fromhex('9204a0') + b'\x00' * 16
N_RESP = 96

# job -> {param_index: value, ...}
RUNS = [
    ('FLASH_LESEN',          {1: 0x038000, 2: 0x80}),        # p1 = L addr, p2 = I count
    ('BAUDRATEN_UMSTELLUNG', {1: 50000,    2: 0}),           # p1 = L (new baud?), p2 = I
    ('BAUDRATEN_LESEN',      {1: 0}),                       # p1 = I
    ('SET_EDIC_BAUDRATE',    {1: b'50000'}),               # p1 = string
    ('AIF_LESEN',            {1: b'0'}),                   # p1 = string (index?)
    ('AIF_SCHREIBEN',        {1: b'0', 2: b'\x00' * 8}),   # p1 = string, p2 = data?
    ('STEUERN_SYNC_MODE',    {1: b'0'}),                   # p1 = string
]


def run(prg, name, params):
    vm = VM(prg)
    vm.params.update(params)
    for _ in range(N_RESP):
        vm.responses.append(bytes(GENERIC))
    err = None
    try:
        vm.run(name)
    except (RuntimeError, NotImplementedError, IndexError, KeyError, ValueError,
            OverflowError, ZeroDivisionError) as e:
        err = f"{type(e).__name__}: {e}"
    reqs = [(p, bytes(r)) for p, r in vm.requests]
    dedup = []
    for p, r in reqs:
        if not dedup or dedup[-1][1] != r:
            dedup.append((p, r))
    vars_ = {k: (v.decode('latin1', 'replace') if isinstance(v, (bytes, bytearray))
                 else v) for k, v in vm.vars.items()}
    return reqs, dedup, vars_, err


def main():
    prg = Prg(PRG)
    lines = ["", "## Parameterized runs", ""]
    for name, params in RUNS:
        _, dedup, vars_, err = run(prg, name, params)
        lines.append(f"### {name}  params={params}")
        if err:
            lines.append(f"  - ⚠️ stopped early: `{err}`")
        if not dedup:
            lines.append("  - (no `xsend` captured before stop)")
        for p, r in dedup[:24]:
            lines.append(f"  - `{p:#08x}`  `{r.hex(' ')}`")
        if vars_:
            lines.append(f"  - vars: {vars_}")
        lines.append("")
    text = "\n".join(lines)

    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'analysis', 'frames.md')
    with open(path, 'a') as f:
        f.write(text + "\n")
    print(text)


if __name__ == '__main__':
    main()
