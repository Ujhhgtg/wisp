import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import Image

from gui_smoke import ROOT, dispatch, find_client, restore_clipboard, run, save_clipboard, set_window_property, wait_for


PROBE = r"""
import os, sys, tty
tty.setraw(0)
os.write(1, b'\x1b[2J\x1b[H\x1b[?25l' + (b'\x1b[?2004h' if sys.argv[2] == '1' else b''))
os.write(1, b'\x1b]2;wisp-paste-ready\x07')
with open(sys.argv[1], 'ab', buffering=0) as capture:
    while True:
        capture.write(os.read(0, 4096))
"""


def main():
    artifacts = ROOT / "target/gui-paste-warning"
    artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    monitor = next(monitor for monitor in json.loads(run("hyprctl", "monitors", "-j").stdout) if monitor["focused"])
    clipboards = [save_clipboard(), save_clipboard(primary=True)]
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="wisp-paste-warning-") as temporary:
            directory = Path(temporary)
            config = directory / "config.toml"
            config.write_text('[window]\nmargin_width=0\n[cursor]\nblink=false\nsmooth=false\ntrail=false\n')
            cases = [
                ("ordinary", False, "plain text", None, b"plain text", False),
                ("bracketed-newline", True, "one\ntwo", None, b"\x1b[200~one\ntwo\x1b[201~", False),
                ("legacy-newline", False, "one\ntwo", "enter", b"one\x1bEtwo", False),
                ("sanitize", True, "\x1b[31m\x03", "s", "\x1b[200~␛[31m␃\x1b[201~".encode(), False),
                ("anyway", False, "one\n\x03", "a", b"one\r\x03", False),
                ("cancel", False, "\x1b[31m", "c", b"", False),
                ("mouse", False, "one\ntwo", "mouse", b"one\x1bEtwo", False),
            ]
            for name, bracketed, payload, action, expected, primary in cases:
                result = directory / f"{name}.input"
                log_path = artifacts / f"{name}.log"
                with log_path.open("w") as log:
                    process = subprocess.Popen([str(ROOT / "target/release/wisp"), "--config", str(config), "-e", "python3", "-u", "-c", PROBE, str(result), str(int(bracketed))], env=os.environ | {"WISP_TRACE_TIMING": "1"}, stdout=log, stderr=log)
                client = wait_for(lambda: find_client(process.pid), name + " window")
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                for prop, value in [("opacity", "1"), ("opacity_inactive", "1"), ("opacity_override", "true"), ("opacity_inactive_override", "true"), ("rounding", "0"), ("no_anim", "true")]:
                    set_window_property(address, prop, value)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                dispatch("focuswindow", address)
                wait_for(lambda: (find_client(process.pid) or {}).get("size") == [900, 500], name + " size")
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-paste-ready", name + " ready")
                subprocess.run(["wl-copy", *(["--primary"] if primary else []), "--type", "text/plain;charset=utf-8"], input=payload.encode(), check=True)
                wait_for(lambda: subprocess.run(["wl-paste", "--no-newline", *(["--primary"] if primary else []), "--type", "text/plain;charset=utf-8"], capture_output=True).stdout == payload.encode(), name + " clipboard ready")
                if primary:
                    client = find_client(process.pid)
                    run(str(ROOT / "target/debug/examples/pointer_driver"), "click", str(client["at"][0] + 30), str(client["at"][1] + 30), str(monitor["width"]), str(monitor["height"]), "middle")
                else:
                    run(str(ROOT / "target/debug/examples/keyboard_driver"), "paste")
                if action:
                    wait_for(lambda: "paste-dialog-open" in log_path.read_text(), name + " warning")
                    assert not result.exists() or result.read_bytes() == b"", "unapproved text leaked to child"
                    time.sleep(0.15)
                    client = find_client(process.pid)
                    x, y = client["at"]
                    width, height = client["size"]
                    capture = artifacts / f"{name}.png"
                    run("grim", "-g", f"{x},{y} {width}x{height}", str(capture))
                    if action == "mouse":
                        image = Image.open(capture).convert("RGB")
                        yellow = [(column, row) for row in range(height) for column in range(width) if (lambda pixel: pixel[0] > 120 and pixel[1] > 100 and pixel[2] < 100)(image.getpixel((column, row))) ]
                        frame_row = max({row for _, row in yellow}, key=lambda row: sum(point[1] == row for point in yellow))
                        frame = [point for point in yellow if point[1] == frame_row]
                        left, right = min(point[0] for point in frame), max(point[0] for point in frame)
                        top, bottom = min(point[1] for point in yellow), max(point[1] for point in yellow)
                        run(str(ROOT / "target/debug/examples/pointer_driver"), "click", str(x + (left + right) // 2), str(y + (top + bottom) // 2), str(monitor["width"]), str(monitor["height"]))
                    elif action in ["enter", "escape"]:
                        run(str(ROOT / "target/debug/examples/keyboard_driver"), action)
                    else:
                        run("wtype", action)
                    wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-paste-ready", name + " dismissed")
                else:
                    assert "paste-dialog-open" not in log_path.read_text()
                if expected:
                    wait_for(lambda: result.exists() and result.read_bytes() == expected, name + " exact PTY data")
                else:
                    time.sleep(0.15)
                    assert not result.exists() or result.read_bytes() == b"", "cancel delivered text"
                process.terminate()
                process.wait(timeout=5)
                process = None
                print("PASS " + name + " paste policy, dialog, and exact PTY bytes", flush=True)
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        restore_clipboard(clipboards[0])
        restore_clipboard(clipboards[1], primary=True)
        if active:
            dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
