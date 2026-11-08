import os
import subprocess
import sys
import termios
import tty


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


original = termios.tcgetattr(0)
tty.setraw(0)
title("icat-ready")
os.read(0, 1)
try:
    process = subprocess.run(
        [
            "kitten", "icat", "--engine=builtin", "--stdin=no",
            "--detection-timeout=1", "--transfer-mode=" + sys.argv[1],
            sys.argv[2],
        ],
        check=False,
    )
    title("icat-status:" + str(process.returncode))
    while b"\x04" not in os.read(0, 1024):
        pass
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
