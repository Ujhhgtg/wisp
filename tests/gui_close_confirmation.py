import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from PIL import Image

from gui_smoke import ROOT, dispatch, find_client, run, set_window_property, wait_for


PROBE = r"""
import os, sys, tty
tty.setraw(0)
os.write(1, b'\x1b[2J\x1b[H\x1b[>3u\x1b[?1003h\x1b[?1006h\x1b]2;wisp-close-ready\x07')
with open(sys.argv[1], 'ab', buffering=0) as capture:
    while True:
        capture.write(os.read(0, 4096))
"""


def main():
    artifacts = ROOT / "target/gui-close-confirmation"
    artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    monitor = next(monitor for monitor in json.loads(run("hyprctl", "monitors", "-j").stdout) if monitor["focused"])
    processes = []
    with tempfile.TemporaryDirectory(prefix="wisp-close-confirmation-") as temporary:
        directory = Path(temporary)
        xdg = directory / "xdg"
        (xdg / "fish").mkdir(parents=True)
        (xdg / "fish/config.fish").write_text("set -g fish_history ''; function fish_prompt; printf '\\e]2;wisp-close-ready\\a'; printf 'fish> '; end\n")

        def launch(name, command=None, locale="en_US.UTF-8", enabled=True):
            config = directory / f"{name}.toml"
            config.write_text('[window]\nmargin_width=0\nconfirm_on_close=' + str(enabled).lower() + '\n[cursor]\nblink=false\nsmooth=false\ntrail=false\n')
            environment = os.environ | {"SHELL": "/usr/bin/fish", "XDG_CONFIG_HOME": str(xdg), "LC_ALL": locale, "WISP_TRACE_TIMING": "1"}
            log_path = artifacts / f"{name}.log"
            arguments = [str(ROOT / "target/release/wisp"), "--config", str(config)]
            if command:
                arguments.extend(["-e", *command])
            with log_path.open("w") as log:
                process = subprocess.Popen(arguments, env=environment, stdout=log, stderr=log)
            processes.append(process)
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
            wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-close-ready", name + " prompt")
            time.sleep(0.15)
            return process, log_path

        def request_close(process):
            client = find_client(process.pid)
            response = run("hyprctl", "dispatch", 'hl.dsp.window.close({window=' + json.dumps("address:" + client["address"]) + '})').stdout.decode().strip()
            assert response == "ok", response

        def dialog(process, log_path, count=1):
            request_close(process)
            wait_for(lambda: log_path.read_text().count("\tclose-dialog-open") >= count, "confirmation screen")
            time.sleep(0.12)
            assert process.poll() is None

        def key(name):
            operation = {"Return": "enter", "Escape": "escape", "Tab": "tab", "Left": "left", "Right": "right"}[name]
            run(str(ROOT / "target/debug/examples/keyboard_driver"), operation)
            time.sleep(0.08)

        def screenshot(process, name):
            client = find_client(process.pid)
            x, y = client["at"]
            width, height = client["size"]
            path = artifacts / f"{name}.png"
            run("grim", "-g", f"{x},{y} {width}x{height}", str(path))
            return Image.open(path).convert("RGB")

        def click(process, close):
            client = find_client(process.pid)
            image = screenshot(process, "click-target")
            yellow = [(x, y) for y in range(image.height) for x in range(image.width)
                      if (lambda color: color[0] > 120 and color[1] > 100 and color[2] < 100)(image.getpixel((x, y))) ]
            assert yellow, "selected Kitty frame must be yellow"
            left = min(point[0] for point in yellow)
            right = max(point[0] for point in yellow)
            top = min(point[1] for point in yellow)
            bottom = max(point[1] for point in yellow)
            cell_width = (right - left + 1) / 7
            x = client["at"][0] + (left + right) / 2 + (0 if close else 8.5 * cell_width)
            y = client["at"][1] + (top + bottom) / 2
            run(str(ROOT / "target/debug/examples/pointer_driver"), "click", str(round(x)), str(round(y)), str(monitor["width"]), str(monitor["height"]))

        try:
            process, log = launch("idle-fish")
            request_close(process)
            process.wait(timeout=5)
            assert "close-dialog-open" not in log.read_text()
            print("PASS idle default fish closes without confirmation", flush=True)

            for foreground in [True, False]:
                name = "foreground" if foreground else "background"
                process, log = launch(name)
                run("wtype", "sleep 30" + ("" if foreground else " &"))
                key("Return")
                time.sleep(0.2)
                dialog(process, log)
                key("Escape")
                wait_for(lambda: "close-dialog-cancelled" in log.read_text(), "escape cancellation")
                assert process.poll() is None
                dialog(process, log, 2)
                key("Return")
                process.wait(timeout=5)
                print("PASS fish " + name + " job, Escape cancellation, and Kitty default Yes", flush=True)

            for name, locale in [("english", "en_US.UTF-8"), ("simplified", "zh_CN.UTF-8"), ("traditional", "zh_TW.UTF-8")]:
                capture = directory / f"{name}-input"
                process, log = launch(name, ["python3", "-u", "-c", PROBE, str(capture)], locale)
                dialog(process, log)
                screenshot(process, name)
                baseline = capture.read_bytes() if capture.exists() else b""
                key("Left")
                key("Right")
                key("Escape")
                wait_for(lambda: "close-dialog-cancelled" in log.read_text(), "escape cancellation")
                assert (capture.read_bytes() if capture.exists() else b"") == baseline, "dialog keys leaked to child"
                dialog(process, log, 2)
                before = capture.read_bytes() if capture.exists() else b""
                click(process, False)
                wait_for(lambda: log.read_text().count("close-dialog-cancelled") == 2, "mouse cancellation")
                assert (capture.read_bytes() if capture.exists() else b"") == before, "dialog mouse leaked to child"
                dialog(process, log, 3)
                click(process, True)
                process.wait(timeout=5)
                print("PASS " + name + " dialog, keyboard/mouse isolation, and command confirmation", flush=True)

            process, log = launch("resized", ["python3", "-u", "-c", PROBE, str(directory / "resize-input")])
            dialog(process, log)
            address = "address:" + find_client(process.pid)["address"]
            dispatch("resizewindowpixel", "exact 320 200," + address)
            wait_for(lambda: (find_client(process.pid) or {}).get("size") == [320, 200], "dialog minimum size")
            time.sleep(0.15)
            screenshot(process, "minimum-size")
            click(process, False)
            wait_for(lambda: "close-dialog-cancelled" in log.read_text(), "resized mouse cancellation")
            dialog(process, log, 2)
            run("wtype", "y")
            process.wait(timeout=5)
            print("PASS dialog resize, minimum-size mouse hit testing, and Y shortcut", flush=True)

            process, log = launch("finishing-command", ["/bin/sh", "-c", "printf '\033]2;wisp-close-ready\007'; sleep 1"])
            dialog(process, log)
            process.wait(timeout=5)
            print("PASS command exit closes an open confirmation automatically", flush=True)

            process, log = launch("disabled", ["python3", "-u", "-c", PROBE, str(directory / "disabled-input")], enabled=False)
            request_close(process)
            process.wait(timeout=5)
            assert "close-dialog-open" not in log.read_text()
            print("PASS confirm_on_close=false bypasses confirmation", flush=True)
        finally:
            for process in processes:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
            if active:
                dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
