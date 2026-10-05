#!/usr/bin/env python3
"""Pushes kitty keyboard flags (argv[1], default 1) and prints each read that arrives as escaped
text, `key <bytes>`, so `rec screen` can check what a key became. Pops the flags and exits on q."""
import os
import sys
import termios
import tty

flags = int(sys.argv[1]) if len(sys.argv) > 1 else 1
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
out = sys.stdout.fileno()
os.write(out, f"\x1b[>{flags}uprobe ready\r\n".encode())
try:
    while True:
        data = os.read(fd, 1024)
        if data == b"q":
            break
        shown = data.decode("latin-1").encode("unicode_escape").decode()
        os.write(out, f"key {shown}\r\n".encode())
finally:
    os.write(out, b"\x1b[<u")
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
