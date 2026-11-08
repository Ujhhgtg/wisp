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


def set_window_property(address, name, value):
    expression = f'hl.dsp.window.set_prop({{window={json.dumps(address)},prop={json.dumps(name)},value={json.dumps(value)},lock=true}})'
    response = run("hyprctl", "dispatch", expression).stdout.decode().strip()
    assert response == "ok", (name, response)


def find_client(pid):
    return next((client for client in clients() if client["pid"] == pid), None)


def ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def save_clipboard(primary=False):
    selection = ["--primary"] if primary else []
    types = subprocess.run(
        ["wl-paste", *selection, "--list-types"], capture_output=True, timeout=2
    )
    if types.returncode:
        return None
    names = types.stdout.decode().splitlines()
    for name in ["text/plain;charset=utf-8", "text/plain", *names]:
        if name not in names:
            continue
        data = subprocess.run(
            ["wl-paste", *selection, "--type", name, "--no-newline"],
            capture_output=True,
            timeout=2,
        )
        if data.returncode == 0:
            return name, data.stdout
    raise AssertionError("unable to preserve the existing clipboard before testing copy/paste")


def restore_clipboard(saved, primary=False):
    selection = ["--primary"] if primary else []
    if saved is None:
        arguments, data = ["wl-copy", *selection, "--clear"], None
    else:
        kind, data = saved
        arguments = ["wl-copy", *selection, "--type", kind]
    subprocess.run(
        arguments,
        input=data,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=2,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--pointer-driver", type=Path, default=ROOT / "target/debug/examples/pointer_driver")
    parser.add_argument("--keyboard-driver", type=Path, default=ROOT / "target/debug/examples/keyboard_driver")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/gui-smoke")
    options = parser.parse_args()
    options.artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    monitor = next(monitor for monitor in json.loads(run("hyprctl", "monitors", "-j").stdout) if monitor["focused"])
    display_width = monitor["width"]
    display_height = monitor["height"]
    try:
        smoke(options, monitor, display_width, display_height)
    finally:
        if active:
            dispatch("focuswindow", "address:" + active)


def smoke(options, monitor, display_width, display_height):
    assert options.binary.exists(), f"build {options.binary} before running the GUI smoke"
    assert options.pointer_driver.exists(), "run cargo build --example pointer_driver first"
    assert options.keyboard_driver.exists(), "run cargo build --example keyboard_driver first"
    with tempfile.TemporaryDirectory(prefix="wisp-gui-") as temporary:
        directory = Path(temporary)
        config = directory / "config.toml"
        config.write_text('[font]\nfamily="FiraCode Nerd Font"\nsize=11\n[window]\nmargin_width=21.75\n[colors]\nbackground="#282c34"\nforeground="#abb2bf"\n[cursor]\nblink=false\nsmooth=false\ntrail=false\n')
        opened = directory / "opened-url"
        opener = directory / "xdg-open"
        opener.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$WISP_OPEN_LOG"\n')
        opener.chmod(0o755)
        environment = os.environ | {"PATH": str(directory) + ":" + os.environ["PATH"], "WISP_OPEN_LOG": str(opened)}

        def launch(mode, image="", result=None):
            control = directory / mode
            control.mkdir()
            arguments = [str(options.binary.resolve()), "--config", str(config), "-e", "python3", "-u", str(ROOT / "tests/fixtures/gui_scene.py"), mode, image, str(result or ""), "--control-dir", str(control)]
            log = (options.artifacts / f"{mode}.log").open("w")
            process = subprocess.Popen(arguments, env=environment, stdout=log, stderr=log)
            try:
                client = wait_for(lambda: find_client(process.pid), "mapped " + mode)
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                for name, value in [
                    ("opacity", "1"),
                    ("opacity_inactive", "1"),
                    ("opacity_override", "true"),
                    ("opacity_inactive_override", "true"),
                    ("rounding", "0"),
                    ("no_anim", "true"),
                ]:
                    set_window_property(address, name, value)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                wait_for(lambda: (find_client(process.pid) or {}).get("size") == [900, 500], mode + " final size")
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-gui-starting", mode + " fixture startup")
                focus(process)
                (control / "start").touch()
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-gui-ready", mode + " ready")
                geometry = json.loads(wait_for(lambda: (control / "geometry.json").read_text() if (control / "geometry.json").exists() else None, mode + " geometry"))
                assert geometry["pixel_width"] == geometry["columns"] * geometry["cell_width"], geometry
                assert geometry["pixel_height"] == geometry["rows"] * geometry["cell_height"], geometry
                client = find_client(process.pid)
                geometry["origin_x"] = (client["size"][0] - geometry["pixel_width"]) // 2
                geometry["origin_y"] = (client["size"][1] - geometry["pixel_height"]) // 2
                margin = round(21.75 * 96 / 72 * monitor["scale"])
                assert margin <= geometry["origin_x"] < margin + geometry["cell_width"], geometry
                assert margin <= geometry["origin_y"] < margin + geometry["cell_height"], geometry
                (options.artifacts / f"{mode}-geometry.json").write_text(json.dumps(geometry, indent=2) + "\n")
                time.sleep(0.15)
                return process, log, client, geometry
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

        def focus(process):
            client = find_client(process.pid)
            assert client, "test window exited before receiving input"
            x, y = client["at"]
            width, height = client["size"]
            pointer("move", x + width - 40, y + height - 40, display_width, display_height)
            dispatch("focuswindow", "address:" + client["address"])
            wait_for(lambda: json.loads(run("hyprctl", "activewindow", "-j").stdout).get("pid") == process.pid, "test-window input focus")

        def keyboard(process, *arguments):
            focus(process)
            run("wtype", *arguments)

        def shortcut(process, key):
            focus(process)
            run(str(options.keyboard_driver), key)

        def capture(client, name):
            x, y = client["at"]
            width, height = client["size"]
            screenshot = options.artifacts / f"{name}.png"
            run("grim", "-g", f"{x},{y} {width}x{height}", str(screenshot))
            return Image.open(screenshot).convert("RGB")

        def cell(geometry, column, row):
            return (geometry["origin_x"] + column * geometry["cell_width"], geometry["origin_y"] + row * geometry["cell_height"])

        process, log, client, geometry = launch("scene")
        try:
            x, y = client["at"]
            pixels = capture(client, "scene")
            background = (40, 44, 52)
            displayed_background = pixels.getpixel((10, 10))
            assert max(abs(a - b) for a, b in zip(displayed_background, background)) <= 12, displayed_background
            boxes = pixels.load()
            baseline = geometry["origin_y"]
            glyph_points = [(column, row) for row in range(baseline, baseline + geometry["cell_height"]) for column in range(geometry["origin_x"] + 6 * geometry["cell_width"]) if max(abs(a - b) for a, b in zip(boxes[column, row], displayed_background)) > 50]
            assert glyph_points, "configured font produced no visible pixels"
            left = min(point[0] for point in glyph_points)
            assert geometry["origin_x"] <= left < geometry["origin_x"] + geometry["cell_width"], ("margin origin", left, geometry)
            assert max(point[1] for point in glyph_points) - min(point[1] for point in glyph_points) >= 7, "configured font is too small to read"
            for index in range(16):
                column, palette_row = cell(geometry, index * 2, 3)
                _, reference_row = cell(geometry, index * 2, 4)
                palette = pixels.getpixel((column + geometry["cell_width"], palette_row + geometry["cell_height"] // 2))
                reference = pixels.getpixel((column + geometry["cell_width"], reference_row + geometry["cell_height"] // 2))
                assert max(abs(first - second) for first, second in zip(palette, reference)) <= 3, ("ANSI palette", index, palette, reference)
            for row, expected in [(1, "https://example.com/path"), (2, "https://target.example/actual")]:
                link_x, link_y = cell(geometry, 2, row)
                for attempt in range(3):
                    focus(process)
                    pointer("click", x + link_x + geometry["cell_width"] // 2, y + link_y + geometry["cell_height"] // 2, display_width, display_height)
                    time.sleep(0.65)
                    if opened.exists() and opened.read_text().splitlines()[-1] == expected:
                        break
                else:
                    raise AssertionError(("hyperlink click", expected, run("hyprctl", "cursorpos").stdout.decode()))
            print("PASS window, configured font, background, margin, ANSI palette, URL and OSC8 clicks")
        finally:
            stop(process, log)

        result = directory / "received-mouse"
        process, log, client, geometry = launch("mouse", result=result)
        try:
            x, y = client["at"]
            mouse_x, mouse_y = cell(geometry, 4, 2)
            mouse_x += x + geometry["cell_width"] // 2
            mouse_y += y + geometry["cell_height"] // 2
            pointer("click", mouse_x, mouse_y, display_width, display_height)
            pointer("wheel", mouse_x, mouse_y, display_width, display_height, "up")
            shortcut(process, "eof")
            wait_for(result.exists, "mouse child bytes")
            received = result.read_bytes()
            assert received == b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<64;5;3M", received
            print("PASS actual Wayland pointer -> winit -> PTY SGR bytes", repr(received))
        finally:
            stop(process, log)

        image = directory / "fixture.png"
        Image.new("RGB", (80, 40), (211, 37, 89)).save(image)
        process, log, client, geometry = launch("image", str(image))
        try:
            pixels = capture(client, "icat")
            origin_x, origin_y = cell(geometry, 0, 0)
            reference_x, reference_y = cell(geometry, 19, 0)
            reference = pixels.getpixel((reference_x + geometry["cell_width"] // 2, reference_y + geometry["cell_height"] // 2))
            assert reference[0] > reference[1] * 2 and reference[0] > reference[2], reference
            image_pixels = pixels.crop((origin_x, origin_y, origin_x + 80, origin_y + 40))
            count = sum(max(abs(first - second) for first, second in zip(pixel, reference)) <= 3 for pixel in image_pixels.get_flattened_data())
            assert count == 80 * 40, ("icat image pixels", count, reference)
            print("PASS kitten icat real-window pixels", count)
        finally:
            stop(process, log)

        result = directory / "received-keyboard"
        clipboard = save_clipboard()
        primary = save_clipboard(primary=True)
        process, log, client, geometry = launch("keyboard", result=result)
        try:
            keyboard(process, "wisp123")
            wait_for(lambda: result.with_suffix(".partial").exists() and result.with_suffix(".partial").read_bytes() == b"wisp123", "printable keyboard input")
            x, y = client["at"]
            start_x, start_y = cell(geometry, 0, 0)
            end_x, _ = cell(geometry, 6, 0)
            pointer("drag", x + start_x + geometry["cell_width"] // 4, y + start_y + geometry["cell_height"] // 2, x + end_x + geometry["cell_width"] * 3 // 4, y + start_y + geometry["cell_height"] // 2, display_width, display_height)
            capture(client, "selection")
            shortcut(process, "copy")
            wait_for(lambda: subprocess.run(["wl-paste", "--no-newline"], capture_output=True, timeout=2).stdout == b"wisp123", "Ctrl+Shift+C clipboard text")
            shortcut(process, "paste")
            wait_for(lambda: result.with_suffix(".partial").read_bytes() == b"wisp123wisp123", "Ctrl+Shift+V PTY paste")
            shortcut(process, "eof")
            wait_for(result.exists, "control-key delivery")
            assert result.read_bytes() == b"wisp123wisp123", result.read_bytes()
            print("PASS actual Wayland keyboard, selection, Ctrl+Shift+C/V, and PTY control input")
        finally:
            stop(process, log)
            restore_clipboard(clipboard)
            restore_clipboard(primary, primary=True)

        process, log, client, geometry = launch("resize")
        try:
            control = directory / "resize" / "resize.json"
            before = json.loads(control.read_text())
            dispatch("resizewindowpixel", "exact 1080 614,address:" + client["address"])
            changed = wait_for(lambda: json.loads(control.read_text()) if control.exists() and json.loads(control.read_text()) != before else None, "PTY resize notification")
            assert changed["columns"] > before["columns"] and changed["rows"] > before["rows"], (before, changed)
            assert changed["pixel_width"] == changed["columns"] * geometry["cell_width"], changed
            assert changed["pixel_height"] == changed["rows"] * geometry["cell_height"], changed
            wait_for(lambda: (find_client(process.pid) or {}).get("size") == [1080, 614], "resized window dimensions")
            time.sleep(0.15)
            client = find_client(process.pid)
            pixels = capture(client, "resize")
            right = (client["size"][0] + changed["pixel_width"]) // 2
            bottom = (client["size"][1] + changed["pixel_height"]) // 2
            corner = pixels.crop((right - geometry["cell_width"], bottom - geometry["cell_height"], right, bottom))
            assert any(max(pixel) > 100 for pixel in corner.get_flattened_data()), "resized last-cell marker is missing"
            print("PASS actual window resize -> PTY dimensions, SIGWINCH, and last-cell rendering")
        finally:
            stop(process, log)

        process, log, client, geometry = launch("idle")
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


if __name__ == "__main__":
    main()
