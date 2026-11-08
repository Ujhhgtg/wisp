import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import termios
import time
import tty


PALETTE = [
    (0, 0, 0), (204, 0, 0), (78, 154, 6), (196, 160, 0),
    (52, 101, 164), (117, 80, 123), (6, 152, 154), (211, 215, 207),
    (85, 87, 83), (239, 41, 41), (138, 226, 52), (252, 233, 79),
    (114, 159, 207), (173, 127, 168), (52, 226, 226), (238, 238, 236),
]


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


def geometry():
    dimensions = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    os.write(1, b"\x1b[16t")
    response = bytearray()
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if select.select([0], [], [], 0.1)[0]:
            response.extend(os.read(0, 4096))
            match = re.search(rb"\x1b\[6;(\d+);(\d+)t", response)
            if match:
                height, width = map(int, match.groups())
                return dict(
                    rows=dimensions[0], columns=dimensions[1],
                    pixel_width=dimensions[2], pixel_height=dimensions[3],
                    cell_width=width, cell_height=height,
                )
    raise RuntimeError("terminal did not answer its cell-size query")


def resized(*_):
    dimensions = struct.unpack("HHHH", fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    if options.control_dir:
        size = dict(
            rows=dimensions[0], columns=dimensions[1],
            pixel_width=dimensions[2], pixel_height=dimensions[3],
        )
        (options.control_dir / "resize.json").write_text(json.dumps(size))
    os.write(1, f"\x1b[2J\x1b[Hresize\x1b[{dimensions[0]};{dimensions[1]}HZ\x1b[?25l".encode())


parser = argparse.ArgumentParser()
parser.add_argument("mode")
parser.add_argument("image", nargs="?", default="")
parser.add_argument("result", nargs="?", default="")
parser.add_argument("--control-dir", type=Path)
options = parser.parse_args()
original = termios.tcgetattr(0)
tty.setraw(0)
try:
    mode = options.mode
    if options.control_dir:
        title("wisp-gui-starting")
        deadline = time.monotonic() + 8
        while not (options.control_dir / "start").exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("GUI harness did not release the scene")
            time.sleep(0.01)
        (options.control_dir / "geometry.json").write_text(json.dumps(geometry()))
    os.write(1, b"\x1b[2J\x1b[H")
    if mode == "scene":
        os.write(1, b"MM grid\r\nhttps://example.com/path\r\n")
        os.write(1, b"\x1b]8;;https://target.example/actual\x1b\\explicit-link\x1b]8;;\x1b\\\r\n")
        for number in range(16):
            os.write(1, f"\x1b[48;5;{number}m  ".encode())
        os.write(1, b"\x1b[0m\r\n")
        for red, green, blue in PALETTE:
            os.write(1, f"\x1b[48;2;{red};{green};{blue}m  ".encode())
        os.write(1, b"\x1b[0m\x1b[H\x1b[?25l")
    elif mode == "image":
        subprocess.run(
            ["kitten", "icat", "--engine=builtin", "--stdin=no", "--align=left", "--transfer-mode=stream", options.image],
            check=True,
        )
        os.write(1, b"\x1b[1;20H\x1b[48;2;211;37;89m  \x1b[0m\x1b[?25l")
    elif mode == "mouse":
        os.write(1, b"\x1b[?1000h\x1b[?1006h")
    elif mode == "idle":
        os.write(1, b"idle\x1b[?25l")
    elif mode == "resize":
        signal.signal(signal.SIGWINCH, resized)
        resized()
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
        if options.result:
            Path(options.result + ".partial").write_bytes(received)
        if mode in ("echo", "keyboard"):
            os.write(1, chunk)
    if options.result:
        with open(options.result, "wb") as output:
            output.write(received)
    title("wisp-gui-finished")
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
