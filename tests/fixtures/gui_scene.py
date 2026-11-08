import os
import select
import subprocess
import sys
import termios
import time
import tty


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


original = termios.tcgetattr(0)
tty.setraw(0)
try:
    mode = sys.argv[1]
    os.write(1, b"\x1b[2J\x1b[H")
    if mode == "scene":
        os.write(1, b"MM grid\r\nhttps://example.com/path\r\n")
        os.write(1, b"\x1b]8;;https://target.example/actual\x1b\\explicit-link\x1b]8;;\x1b\\\r\n")
        for number in range(16):
            os.write(1, f"\x1b[48;5;{number}m  ".encode())
        os.write(1, b"\x1b[0m\x1b[H\x1b[?25l")
    elif mode == "image":
        subprocess.run(["kitten", "icat", "--stdin=no", sys.argv[2]], check=True)
        os.write(1, b"\x1b[?25l")
    elif mode == "mouse":
        os.write(1, b"\x1b[?1000h\x1b[?1006h")
    elif mode == "idle":
        os.write(1, b"idle\x1b[?25l")
    title("wisp-gui-ready")
    received = bytearray()
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        ready, _, _ = select.select([0], [], [], 1)
        if not ready:
            continue
        chunk = os.read(0, 4096)
        if b"\x04" in chunk:
            received.extend(chunk[: chunk.index(b"\x04")])
            break
        received.extend(chunk)
        if mode == "echo":
            os.write(1, chunk)
    if len(sys.argv) > 3:
        with open(sys.argv[3], "wb") as output:
            output.write(received)
    title("wisp-gui-finished")
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
