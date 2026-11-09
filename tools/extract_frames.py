#!/usr/bin/env python3
"""Run every job in the VM with a generic OK response queue and capture the
xsend request frames each one builds. Writes analysis/frames.md.

The generic response is `92 04 A0` (status OKAY at byte [2]) followed by zeros,
so frames that a job patches from the response show zero data fields (we only
care about frame structure here). Data values are not meaningful from a
simulated response; the on-wire values come from a real ECU.
"""
import os
import sys
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prg_vm import Prg, VM  # noqa: E402

PRG = os.environ.get('DDE3_PRG', '/home/ilija/Desktop/logger/10DDE30.prg')
GENERIC = bytes.fromhex('9204a0') + b'\x00' * 16   # 92 04 A0 (OKAY) + zeros
N_RESP = 96                                         # keep the queue fed through loops


def run_job(prg, name):
    vm = VM(prg)
    for _ in range(N_RESP):
        vm.responses.append(bytes(GENERIC))
    err = None
    try:
        vm.run(name)
    except (RuntimeError, NotImplementedError, IndexError, KeyError, ValueError,
            OverflowError, ZeroDivisionError) as e:
        err = f"{type(e).__name__}: {e}"
    reqs = [(p, bytes(r)) for p, r in vm.requests]
    # de-dup CONSECUTIVE identical frames (retry / block-loop noise)
    dedup = []
    for p, r in reqs:
        if not dedup or dedup[-1][1] != r:
            dedup.append((p, r))
    vars_ = {k: (v.decode('latin1', 'replace') if isinstance(v, (bytes, bytearray))
                 else v) for k, v in vm.vars.items()}
    return reqs, dedup, vars_, err


def main():
    prg = Prg(PRG)
    out = []
    out.append("# DDE3 protocol frames (extracted via `tools/prg_vm.py`)")
    out.append("")
    out.append("Generic OK response fed to each job: `92 04 A0` + zero padding.")
    out.append("Frames a job patches from the response show `00` data fields —")
    out.append("this captures the frame **structure** (address / service / length),")
    out.append("not the real payload values.")
    out.append("")
    summary = {}
    for name, addr in prg.jobs.items():
        reqs, dedup, vars_, err = run_job(prg, name)
        summary[name] = (len(reqs), len(dedup), err)
        out.append(f"## {name}  `@ {addr:#06x}`")
        if err:
            out.append(f"  - ⚠️ stopped early: `{err}`")
        if not dedup:
            out.append("  - (no `xsend` captured before stop)")
        for p, r in dedup:
            out.append(f"  - `{p:#08x}`  `{r.hex(' ')}`")
        if vars_:
            out.append(f"  - vars: {json.dumps(vars_, ensure_ascii=False)}")
        out.append("")
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'analysis', 'frames.md')
    with open(path, 'w') as f:
        f.write("\n".join(out))

    print(f"wrote {path}\n")
    print(f"{'job':32s} {'total':>6s} {'dedup':>6s}  error")
    for name, (tot, dd, err) in summary.items():
        flag = " " if not err else "!"
        print(f"{name:32s} {tot:6d} {dd:6d}  {err or ''}")


if __name__ == '__main__':
    main()
