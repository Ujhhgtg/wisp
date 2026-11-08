import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import Image

from gui_smoke import ROOT, dispatch, find_client, run, wait_for


PROBE = r"""
import base64
import os
from pathlib import Path
import sys
import time

directory = Path(sys.argv[1])
os.write(1, b'\x1b]2;wisp-graphics-waiting\x1b\\')
while not (directory / 'start').exists():
    time.sleep(0.01)

def send(controls, color=None):
    data = b'' if color is None else bytes(color) * 4
    os.write(1, b'\x1b_G' + controls.encode() + b';' + base64.b64encode(data) + b'\x1b\\')

os.write(1, b'\x1b[2J\x1b[H\x1b[?25l')
send('a=T,i=42,p=7,f=32,s=2,v=2,U=1,c=4,r=2,q=2', (255, 0, 0, 255))
os.write(1, b'\x1b[3;3H\x1b[38;5;42;58;5;7m')
os.write(1, ('\U0010eeee\u0305\u0305' + '\U0010eeee' * 3).encode())
os.write(1, b'\x1b[4;3H')
os.write(1, ('\U0010eeee\u030d\u0305' + '\U0010eeee' * 3).encode())
os.write(1, b'\x1b[m')
send('a=T,i=43,p=1,f=32,s=2,v=2,c=2,r=1,P=42,Q=7,H=7,V=0,q=2', (0, 0, 255, 255))
os.write(1, b'\x1b[3;17H')
send('a=T,i=44,p=1,f=32,s=2,v=2,c=4,r=2,C=1,q=2', (255, 0, 0, 255))
send('a=a,i=44,r=1,z=140,q=2')
send('a=f,i=44,f=32,s=2,v=2,z=140,q=2', (0, 255, 0, 255))
send('a=f,i=44,f=32,s=2,v=2,z=140,q=2', (0, 0, 255, 255))
send('a=a,i=44,s=3,v=1,q=2')
os.write(1, b'\x1b]2;wisp-graphics-ready\x1b\\')
while True:
    time.sleep(0.5)
"""


def dominant_counts(image):
    red = green = blue = 0
    for pixel in image.convert("RGB").get_flattened_data():
        red += pixel[0] > 130 and pixel[0] > pixel[1] * 2 and pixel[0] > pixel[2] * 2
        green += pixel[1] > 130 and pixel[1] > pixel[0] * 2 and pixel[1] > pixel[2] * 2
        blue += pixel[2] > 130 and pixel[2] > pixel[0] * 2 and pixel[2] > pixel[1] * 2
    return [red, green, blue]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/gui-graphics")
    options = parser.parse_args()
    options.artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="wisp-gui-graphics-") as temporary:
            directory = Path(temporary)
            config = directory / "config.toml"
            config.write_text('[font]\nfamily="FiraCode Nerd Font"\nsize=11\n[window]\nmargin_width=0\n[colors]\nbackground="#000000"\n[cursor]\nblink=false\nsmooth=false\ntrail=false\n')
            with (options.artifacts / "graphics.log").open("w") as log:
                process = subprocess.Popen(
                    [str(options.binary.resolve()), "--config", str(config), "-e", "python3", "-u", "-c", PROBE, str(directory)],
                    stdout=log,
                    stderr=log,
                )
                client = wait_for(lambda: find_client(process.pid), "graphics window")
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-graphics-waiting", "graphics child")
                time.sleep(0.25)
                (directory / "start").touch()
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-graphics-ready", "graphics initialization")
                client = find_client(process.pid)
                x, y = client["at"]
                width, height = client["size"]
                samples = []
                for frame in range(24):
                    capture = options.artifacts / f"frame-{frame:02}.png"
                    run("grim", "-g", f"{x},{y} {width}x{height}", str(capture))
                    image = Image.open(capture)
                    static = dominant_counts(image.crop((0, 0, 120, 180)))
                    animation = dominant_counts(image.crop((120, 0, 250, 180)))
                    assert static[0] > 100, ("Unicode placeholder red pixels", frame, static)
                    assert static[2] > 50, ("relative-placement blue pixels", frame, static)
                    samples.append(animation)
                    time.sleep(0.04)
                visible = [max(range(3), key=sample.__getitem__) for sample in samples if max(sample) > 100]
                assert set(visible) == {0, 1, 2}, ("all animation frame colors", samples)
                assert sum(first != second for first, second in zip(visible, visible[1:])) >= 3, visible
                report = {"window_size": client["size"], "animation_colors": visible, "pixel_counts": samples}
                (options.artifacts / "result.json").write_text(json.dumps(report, indent=2) + "\n")
                print("PASS real-window Unicode placeholders, relative placement, and red/green/blue animation", visible)
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        if active:
            dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
