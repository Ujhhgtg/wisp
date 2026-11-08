import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import Image


ROOT = Path(__file__).resolve().parents[1]


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, **kwargs)


def clients():
    return json.loads(run("hyprctl", "clients", "-j").stdout)


def wait_for(predicate, label, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("timed out waiting for " + label)


def dispatch(*arguments):
    operation, argument = arguments
    if operation == "togglefloating":
        expression = f'hl.dsp.window.float({{window={json.dumps(argument)},action="set"}})'
    elif operation == "focuswindow":
        expression = f'hl.dsp.focus({{window={json.dumps(argument)}}})'
    else:
        coordinates, window = argument.split(",", 1)
        _, x, y = coordinates.split()
        method = "resize" if operation == "resizewindowpixel" else "move"
        expression = f'hl.dsp.window.{method}({{window={json.dumps(window)},x={x},y={y},relative=false}})'
    return run("hyprctl", "dispatch", expression)


def find_client(pid):
    return next((client for client in clients() if client["pid"] == pid), None)


def ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--pointer-driver", type=Path, default=ROOT / "target/debug/examples/pointer_driver")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/gui-smoke")
    options = parser.parse_args()
    options.artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    monitor = next(monitor for monitor in json.loads(run("hyprctl", "monitors", "-j").stdout) if monitor["focused"])
    display_width = monitor["width"]
    display_height = monitor["height"]
    with tempfile.TemporaryDirectory(prefix="wisp-gui-") as temporary:
        directory = Path(temporary)
        config = directory / "config.toml"
        config.write_text('[font]\nfamily="FiraCode Nerd Font"\nsize=11\n[colors]\nbackground="#282c34"\nforeground="#abb2bf"\n[cursor]\nblink=false\nsmooth=false\ntrail=false\n')
        opened = directory / "opened-url"
        opener = directory / "xdg-open"
        opener.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$WISP_OPEN_LOG"\n')
        opener.chmod(0o755)
        environment = os.environ | {"PATH": str(directory) + ":" + os.environ["PATH"], "WISP_OPEN_LOG": str(opened)}

        def launch(mode, image="", result=None):
            arguments = [str(options.binary), "--config", str(config), "-e", "python3", "-u", str(ROOT / "tests/fixtures/gui_scene.py"), mode, image]
            if result:
                arguments.append(str(result))
            log = (options.artifacts / f"{mode}.log").open("w")
            process = subprocess.Popen(arguments, env=environment, stdout=log, stderr=log)
            try:
                client = wait_for(lambda: find_client(process.pid), "mapped " + mode)
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                dispatch("focuswindow", address)
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-gui-ready", mode + " ready")
                time.sleep(1.0)
                return process, log, find_client(process.pid)
            except BaseException:
                process.terminate()
                process.wait(timeout=5)
                log.close()
                raise

        def stop(process, log):
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            log.close()

        def pointer(operation, *coordinates):
            run(str(options.pointer_driver), operation, *map(str, coordinates))

        process, log, client = launch("scene")
        try:
            x, y = client["at"]
            width, height = client["size"]
            screenshot = options.artifacts / "scene.png"
            run("grim", "-g", f"{x},{y} {width}x{height}", str(screenshot))
            pixels = Image.open(screenshot).convert("RGB")
            background = (40, 44, 52)
            displayed_background = pixels.getpixel((10, 10))
            assert max(abs(a - b) for a, b in zip(displayed_background, background)) <= 48, displayed_background
            boxes = pixels.load()
            glyph_points = [(column, row) for row in range(120) for column in range(120) if max(abs(a - b) for a, b in zip(boxes[column, row], displayed_background)) > 50]
            assert glyph_points, "configured font produced no visible pixels"
            left = min(point[0] for point in glyph_points)
            assert 29 <= left <= 39, ("margin origin", left)
            occupied_rows = sorted(set(point[1] for point in glyph_points))
            bands = []
            for row in occupied_rows:
                if bands and row == bands[-1][-1] + 1:
                    bands[-1].append(row)
                else:
                    bands.append([row])
            assert len(bands) >= 3, bands
            for band, expected in [(bands[1], "https://example.com/path"), (bands[2], "https://target.example/actual")]:
                for attempt in range(3):
                    pointer("click", x + 55, y + band[len(band) // 2], display_width, display_height)
                    time.sleep(1.0)
                    if opened.exists() and opened.read_text().splitlines()[-1] == expected:
                        break
                else:
                    raise AssertionError(("hyperlink click", expected, run("hyprctl", "cursorpos").stdout.decode()))
            print("PASS window, configured font, background, margin, URL and OSC8 clicks")
        finally:
            stop(process, log)

        result = directory / "received-mouse"
        process, log, client = launch("mouse", result=result)
        try:
            x, y = client["at"]
            pointer("click", x + 65, y + 70, display_width, display_height)
            pointer("wheel", x + 65, y + 70, display_width, display_height, "up")
            run("wtype", "-M", "ctrl", "-k", "d", "-m", "ctrl")
            wait_for(result.exists, "mouse child bytes")
            received = result.read_bytes()
            assert received.startswith(b"\x1b[<0;"), received
            assert b"\x1b[<64;" in received, received
            assert b"m" in received, received
            print("PASS actual Wayland pointer -> winit -> PTY SGR bytes", repr(received))
        finally:
            stop(process, log)

        image = directory / "fixture.png"
        Image.new("RGB", (80, 40), (211, 37, 89)).save(image)
        process, log, client = launch("image", str(image))
        try:
            x, y = client["at"]
            width, height = client["size"]
            screenshot = options.artifacts / "icat.png"
            run("grim", "-g", f"{x},{y} {width}x{height}", str(screenshot))
            pixels = Image.open(screenshot).convert("RGB")
            count = sum(pixel[0] > 150 and pixel[1] < 90 and 50 < pixel[2] < 120 for pixel in pixels.get_flattened_data())
            assert count >= 75 * 35, ("icat image pixels", count)
            print("PASS kitten icat real-window pixels", count)
        finally:
            stop(process, log)

        process, log, client = launch("idle")
        try:
            time.sleep(0.5)
            start = ticks(process.pid)
            time.sleep(2)
            elapsed_ticks = ticks(process.pid) - start
            cpu_seconds = elapsed_ticks / os.sysconf("SC_CLK_TCK")
            assert cpu_seconds <= 0.15, cpu_seconds
            print("PASS idle CPU seconds over two seconds", cpu_seconds)
        finally:
            stop(process, log)
    if active:
        dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
