import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import Image, ImageChops

from gui_smoke import ROOT, dispatch, find_client, run, wait_for


PROBE = r"""
import os
from pathlib import Path
import subprocess
import sys
import time

directory = Path(sys.argv[1])
os.write(1, b'\x1b]2;wisp-icat-waiting\x1b\\')
while not (directory / 'start').exists():
    time.sleep(0.01)
os.write(1, b'\x1b[2J\x1b[H\x1b[?25l')
status = subprocess.run(['kitten', 'icat', sys.argv[2]], check=False).returncode
os.write(1, f'\x1b]2;wisp-icat-exited:{status}\x1b\\'.encode())
while True:
    time.sleep(0.5)
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("image", type=Path)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/gui-icat-animation")
    options = parser.parse_args()
    options.artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="wisp-icat-animation-") as temporary:
            directory = Path(temporary)
            config = directory / "config.toml"
            config.write_text('[window]\nmargin_width=0\n[cursor]\nblink=false\n')
            with (options.artifacts / "icat.log").open("w") as log:
                process = subprocess.Popen([
                    str(options.binary.resolve()), "--config", str(config), "-e",
                    "python3", "-u", "-c", PROBE, str(directory), str(options.image.resolve()),
                ], stdout=log, stderr=log)
                client = wait_for(lambda: find_client(process.pid), "icat window")
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-icat-waiting", "icat ready")
                time.sleep(0.25)
                (directory / "start").touch()
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-icat-exited:0", "icat successful exit")
                client = find_client(process.pid)
                x, y = client["at"]
                width, height = client["size"]
                frames = set()
                transitions = 0
                previous = None
                for index in range(36):
                    capture = options.artifacts / f"frame-{index:02}.png"
                    run("grim", "-g", f"{x},{y} {width}x{height}", str(capture))
                    with Image.open(capture) as screenshot:
                        pixels = screenshot.convert("RGB").crop((width // 2 - 100, 20, width // 2 + 100, 160))
                    frames.add(hashlib.sha256(pixels.tobytes()).hexdigest())
                    if previous is not None:
                        changed = sum(max(pixel) > 20 for pixel in ImageChops.difference(previous, pixels).get_flattened_data())
                        transitions += changed > 50
                    previous = pixels
                    time.sleep(0.06)
                assert len(frames) >= 4 and transitions >= 4, ("animation stayed static", len(frames), transitions)
                result = {"image": str(options.image.resolve()), "distinct_captures": len(frames), "changed_frames": transitions}
                (options.artifacts / "result.json").write_text(json.dumps(result, indent=2) + "\n")
                print("PASS default kitten icat animation after client exit", result)
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        if active:
            dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
