#!/usr/bin/env python3
"""Asks the terminal it runs in the startup queries Claude Code sends, plus DSR, then prints each
reply as escaped text, one per line, so `rec wait --text` can check them on the screen."""
import os
import select
import sys
import termios
import tty

QUERIES = [
    ("da1", "\x1b[c"),
    ("kitty", "\x1b[?u"),
    ("xtversion", "\x1b[>0q"),
    ("da2", "\x1b[>c"),
    ("dsr", "\x1b[5n"),
    ("cpr", "\x1b[6n"),
]


def read_reply(fd, timeout=1.0):
    out = b""
    while True:
        r, _, _ = select.select([fd], [], [], timeout if not out else 0.05)
        if not r:
            return out
        out += os.read(fd, 1024)


def main():
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    tty.setraw(fd)
    try:
        replies = []
        for name, q in QUERIES:
            os.write(sys.stdout.fileno(), q.encode())
            replies.append((name, read_reply(fd)))
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
    for name, reply in replies:
        print(f"{name}={reply.decode('latin-1').encode('unicode_escape').decode()}")
    print("probe done")
    sys.stdin.readline()


main()
