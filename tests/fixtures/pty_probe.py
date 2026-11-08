import os
import select
import sys
import termios
import time
import tty


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


scenario = sys.argv[1]
original = termios.tcgetattr(0)
tty.setraw(0)
try:
    if scenario == "mouse":
        os.write(1, b"\x1b[?1000h\x1b[?1006h")
    elif scenario == "drag":
        os.write(1, b"\x1b[?1002h\x1b[?1006h")
    elif scenario == "focus":
        os.write(1, b"\x1b[?1004h")
    elif scenario == "paste":
        os.write(1, b"\x1b[?2004h")
    elif scenario == "kitty":
        os.write(1, b"\x1b[>3u")
    elif scenario == "queries":
        os.write(1, b"\x1b[7;13H\x1b[6n\x1b[5n")
    elif scenario == "size":
        os.write(1, b"\x1b[14t\x1b[16t\x1b[18t")
    elif scenario == "selection":
        os.write(1, b"\x1b[2J\x1b[Hhello world")
    title("probe-ready")

    received = bytearray()
    deadline = time.monotonic() + 10
    complete = False
    while time.monotonic() < deadline:
        ready, _, _ = select.select([0], [], [], max(0, deadline - time.monotonic()))
        if not ready:
            break
        chunk = os.read(0, 4096)
        if not chunk:
            break
        if b"\x04" in chunk:
            received.extend(chunk[: chunk.index(b"\x04")])
            complete = True
            break
        received.extend(chunk)
    title("probe-result:" + received.hex() if complete else "probe-timeout")
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
