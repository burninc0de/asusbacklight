#!/usr/bin/env python3
"""Inject fake input events into a real input device node for testing.

The daemon listens on /dev/input/event* and treats key/mouse events as
"user activity". Non-root users in the `input` group may write to these
nodes, so this is handy for testing asus-backlight-idle without a real
keyboard/mouse.

Usage:
    inject-event.py <dev> <type> <code> <value> [value2]

    type:  1=EV_KEY  2=EV_REL  3=EV_ABS
    code:  numeric (e.g. REL_X=0, KEY_A=30) or a name like KEY_A / BTN_LEFT

Examples:
    # simulate a mouse nudge:
    python3 inject-event.py /dev/input/event5 2 0 5
    # simulate one key press and release on the built-in keyboard:
    python3 inject-event.py /dev/input/event2 1 30 1 0
    # simulate a scroll tick:
    python3 inject-event.py /dev/input/event5 2 8 1 0
"""
import os
import struct
import sys

EV_SYN, EV_KEY, EV_REL, EV_ABS = 0, 1, 2, 3

NAMES = {
    "EV_SYN": 0, "EV_KEY": 1, "EV_REL": 2, "EV_ABS": 3,
    "SYN_REPORT": 0, "REL_X": 0, "REL_Y": 1, "REL_WHEEL": 8,
    "ABS_X": 0, "ABS_Y": 1, "ABS_MT_POSITION_X": 53,
    "KEY_A": 30, "KEY_ESC": 1, "KEY_ENTER": 28, "BTN_LEFT": 0x110,
}


def parse_int(s: str) -> int:
    try:
        return int(s, 0)
    except ValueError:
        if s in NAMES:
            return NAMES[s]
        raise


def ev(fd, t, c, v):
    os.write(fd, struct.pack("llHHi", 0, 0, t, c, v))


def main():
    if len(sys.argv) < 5:
        print(__doc__)
        sys.exit(2)
    dev, t, c, v = sys.argv[1], parse_int(sys.argv[2]), parse_int(sys.argv[3]), parse_int(sys.argv[4])
    v2 = parse_int(sys.argv[5]) if len(sys.argv) > 5 else None
    assert os.path.exists(dev), f"{dev} does not exist"
    fd = os.open(dev, os.O_WRONLY)
    try:
        ev(fd, t, c, v)
        ev(fd, EV_SYN, 0, 0)
        if v2 is not None:
            ev(fd, t, c, v2)
            ev(fd, EV_SYN, 0, 0)
    finally:
        os.close(fd)
    print(f"injected {dev}: type={t} code={c} value={v}{' then '+str(v2) if v2 is not None else ''}")


if __name__ == "__main__":
    main()