#!/usr/bin/env python3
"""Turns on mouse reporting and prints what arrives, newest last, so `agentcam screen` can check
which events reached the program at which cells. Exits on q.

  mouse_probe.py          curses with ALL_MOUSE_EVENTS | REPORT_MOUSE_POSITION; prints
                          `ev <col> <row> <names>`.
  mouse_probe.py sgr-any  asks for any-motion SGR reports itself (?1003, ?1006) and prints
                          each one decoded as `sgr <button> <col> <row> <M|m>`, 0-based.
"""

import curses
import os
import re
import sys
import termios
import tty

NAMES = [
    (curses.BUTTON1_PRESSED, "press1"),
    (curses.BUTTON1_RELEASED, "release1"),
    (curses.BUTTON1_CLICKED, "click1"),
    (curses.BUTTON3_PRESSED, "press3"),
    (curses.BUTTON3_RELEASED, "release3"),
    (curses.BUTTON3_CLICKED, "click3"),
    (curses.REPORT_MOUSE_POSITION, "motion"),
]


def with_curses(scr):
    curses.mousemask(curses.ALL_MOUSE_EVENTS | curses.REPORT_MOUSE_POSITION)
    curses.mouseinterval(0)
    scr.addstr(0, 0, "mouse probe ready")
    scr.refresh()
    log = []
    while True:
        ch = scr.getch()
        if ch == ord("q"):
            return
        if ch != curses.KEY_MOUSE:
            continue
        try:
            _, x, y, _, bstate = curses.getmouse()
        except curses.error:
            continue
        names = [n for bit, n in NAMES if bstate & bit] or [hex(bstate)]
        log.append(f"ev {x} {y} {' '.join(names)}")
        scr.erase()
        scr.addstr(0, 0, f"mouse probe ready, {len(log)} events")
        for i, line in enumerate(log[-(curses.LINES - 2):]):
            scr.addstr(i + 2, 0, line)
        scr.refresh()


def sgr_any():
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    tty.setraw(fd)
    out = sys.stdout
    out.write("\x1b[?1003h\x1b[?1006hmouse probe ready\r\n")
    out.flush()
    report = re.compile(rb"\x1b\[<(\d+);(\d+);(\d+)([Mm])")
    buf = b""
    try:
        while True:
            buf += os.read(fd, 1024)
            if b"q" in report.sub(b"", buf):
                return
            end = 0
            for m in report.finditer(buf):
                b, x, y, kind = int(m[1]), int(m[2]) - 1, int(m[3]) - 1, m[4].decode()
                out.write(f"sgr {b} {x} {y} {kind}\r\n")
                end = m.end()
            buf = buf[end:]
            out.flush()
    finally:
        out.write("\x1b[?1003l\x1b[?1006l")
        out.flush()
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)


if sys.argv[1:] == ["sgr-any"]:
    sgr_any()
else:
    curses.wrapper(with_curses)
