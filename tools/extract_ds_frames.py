#!/usr/bin/env python3
"""Run selected jobs of DDE30DS0.prg in the VM with a generic OK response
queue and capture the exact DS2 request frames each one builds.

Writes analysis/ds_frames.md (job list + captured frames)."""
import os
import sys
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prg_vm import Prg, VM  # noqa: E402

PRG = os.environ.get('DDE3_DS_PRG', '/home/ilija/Desktop/logger/DDE30DS0.prg')
GENERIC = bytes.fromhex('9204a0') + b'\x00' * 16   # 92 04 A0 (OKAY) + zeros
N_RESP = 96

# Jobs of interest for the DTC feature; fall back to all jobs if empty.
JOBS = [
    'FS_QUICK_LESEN', 'FS_LESEN', 'FS_LOESCHEN', 'FS_SHADOW_LESEN',
    'PRUEFCODE_LESEN', 'PRUEFSTEMPEL_LESEN', 'EDIC_RESET',
    'EEPROM_LESEN', 'AIF_LESEN', 'IDENT_AIF', 'CODIER_VARIANTE_LESEN',
    'INFO', 'DIAGNOSE_ERHALTEN', 'STATUS_CODIER_CHECKSUMME',
    'SEED_KEY', 'MW_SELECT_LESEN', 'MW_SELECT_LESEN_NORM',
]


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
    dedup = []
    for p, r in reqs:
        if not dedup or dedup[-1][1] != r:
            dedup.append((p, r))
    vars_ = {k: (v.decode('latin1', 'replace') if isinstance(v, (bytes, bytearray))
                 else v) for k, v in vm.vars.items()}
    return reqs, dedup, vars_, err


def main():
    prg = Prg(PRG)
    names = JOBS if JOBS else list(prg.jobs)
    names = [n for n in names if n in prg.jobs]
    out = []
    out.append("# DDE30DS0.prg frames (extracted via `tools/prg_vm.py`)")
    out.append("")
    out.append("Generic OK response fed to each job: `92 04 A0` + zero padding.")
    out.append("")
    out.append("## Job list (177)")
    out.append("")
    for i, name in enumerate(prg.jobs):
        if i % 8 == 0:
            out.append("")
        out.append(name + (', ' if (i + 1) % 8 else ''))
    out.append("")
    for name in names:
        addr = prg.jobs[name]
        reqs, dedup, vars_, err = run_job(prg, name)
        out.append(f"## {name}  `@ {addr:#06x}`")
        if err:
            out.append(f"  - stopped early: `{err}`")
        if not dedup:
            out.append("  - (no `xsend` captured before stop)")
        for p, r in dedup:
            out.append(f"  - `{p:#08x}`  `{r.hex(' ')}`")
        if vars_:
            out.append(f"  - vars: {json.dumps(vars_, ensure_ascii=False)}")
        out.append("")
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'analysis', 'ds_frames.md')
    with open(path, 'w') as f:
        f.write("\n".join(out))
    print(f"wrote {path}")


if __name__ == '__main__':
    main()
