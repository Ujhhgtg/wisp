import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from gui_smoke import ROOT, clients, dispatch, run, wait_for


def main():
    artifacts = ROOT / "target/gui-smoke"
    artifacts.mkdir(parents=True, exist_ok=True)
    log_path = artifacts / "nano-animation.log"
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    monitor = next(monitor for monitor in json.loads(run("hyprctl", "monitors", "-j").stdout) if monitor["focused"])
    with tempfile.TemporaryDirectory(prefix="wisp-nano-") as temporary:
        directory = Path(temporary)
        config = directory / "config.toml"
        config.write_text('[font]\nfamily="FiraCode Nerd Font"\nsize=11\n[cursor]\nblink=false\nsmooth=true\ntrail=true\n')
        document = directory / "document.txt"
        document.write_text(("abcdefghijklmnopqrstuvwxyz0123456789\n") * 8)
        with log_path.open("w") as log:
            process = subprocess.Popen(
                [str(ROOT / "target/release/wisp"), "--config", str(config), "-e", "nano", "--ignorercfiles", "--mouse", str(document)],
                env=os.environ | {"WISP_TRACE_TIMING": "1"},
                stdout=log,
                stderr=log,
            )
            try:
                client = wait_for(lambda: next((client for client in clients() if client["pid"] == process.pid), None), "nano window")
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                dispatch("resizewindowpixel", "exact 900 500," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                dispatch("focuswindow", address)
                time.sleep(1)
                client = next(client for client in clients() if client["pid"] == process.pid)
                x, y = client["at"]
                offset = log_path.stat().st_size
                run(str(ROOT / "target/debug/examples/pointer_driver"), "click", str(x + 170), str(y + 78), str(monitor["width"]), str(monitor["height"]))
                time.sleep(0.4)
                with log_path.open() as source:
                    source.seek(offset)
                    trace = source.read()
                frames = trace.count("\tcursor-animating")
                assert frames >= 2, ("nano mouse must produce intermediate animated cursor frames", trace)
                print("PASS actual nano mouse reposition animates across", frames, "frames")
            finally:
                process.terminate()
                process.wait(timeout=5)
    if active:
        dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
