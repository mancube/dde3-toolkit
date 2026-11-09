#!/usr/bin/env python3
"""Acceptance test for the UPROG_EIN job (see HANDOVER.md §7 / §9).

UPROG_EIN (0x2E7) does two things in sequence:
  1. Sends 12 04 0D (enter programming), reads the status byte (resp[2]),
     looks it up in the JobResult table, and retries if BUSY.
  2. On OKAY it continues into a FLASH_LESEN sequence: builds the template
     12 09 06 <addr lo,mid,hi> <count> (8 bytes), patches the addr/count from
     the UPROG_EIN response, and sends it (xsend @0x680). That second send
     also has its own status lookup, which re-sets the JOB_STATUS result.

Consequences for simulation:
  * A 3-byte response (92 04 A0) is enough to exercise step 1 and the flash
    template build, but too short to supply the addr patch (bytes [3..5]), so
    the flash-read gets an empty response and its 0x00 status overwrites the
    result. This is the documented "design limitation" (HANDOVER §9 bug 7).
  * A >=6-byte UPROG_EIN response plus a second (flash-read) response drives
    the full OKAY path so vm.vars['JOB_STATUS'] == b'OKAY'.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prg_vm import Prg, VM  # noqa: E402

PRG = os.environ.get('DDE3_PRG', '/home/ilija/Desktop/logger/10DDE30.prg')

UPROG_REQ = bytes.fromhex('12040d')          # enter programming
FLASH_TMPL = bytes.fromhex('1209060000000002')  # flash-read template (zero addr)


def run(sim):
    prg = Prg(PRG)
    vm = VM(prg)
    for s in sim:
        vm.responses.append(bytes.fromhex(s))
    vm.run('UPROG_EIN')
    return vm


def test_three_byte_baseline():
    """12 04 0D once, no retry loop, flash template built and sent."""
    vm = run(['9204a0'])
    reqs = [(p, bytes(r)) for p, r in vm.requests]
    uprog = [p for p, r in reqs if r == UPROG_REQ]
    flash = [p for p, r in reqs if r == FLASH_TMPL]
    assert uprog == [0x308], f"12 04 0D must be sent once at 0x308, got {[hex(p) for p in uprog]}"
    assert flash == [0x680], f"flash template must be sent at 0x680, got {[hex(p) for p in flash]}"
    # No retry loop: only the two expected requests, not thousands of 12 04 0D.
    assert len(reqs) == 2, f"expected exactly 2 requests (no retry loop), got {len(reqs)}"
    print("  3-byte baseline: PASS")


def test_full_okay_path():
    """With a realistic UPROG_EIN response + flash-read response -> OKAY."""
    vm = run(['9204a00000000002', '9206a00000000000'])
    assert vm.vars.get('JOB_STATUS') == b'OKAY', \
        f"JOB_STATUS={vm.vars.get('JOB_STATUS')!r} (expected b'OKAY')"
    print("  extended (2 responses) JOB_STATUS == OKAY: PASS")


if __name__ == '__main__':
    print("UPROG_EIN acceptance:")
    test_three_byte_baseline()
    test_full_okay_path()
    print("ALL PASS")
