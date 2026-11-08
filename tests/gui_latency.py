import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from gui_smoke import ROOT, clients, dispatch, run, wait_for


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--idle", type=float, nargs="+", default=[0.1, 1, 3, 10])
    parser.add_argument("--blink", action="store_true")
    options = parser.parse_args()
    artifacts = ROOT / "target/gui-smoke"
    artifacts.mkdir(parents=True, exist_ok=True)
    log_path = artifacts / "typing-latency.log"
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    with tempfile.TemporaryDirectory(prefix="wisp-latency-") as temporary:
        config = Path(temporary) / "config.toml"
        config.write_text('[font]\nfamily="FiraCode Nerd Font"\nsize=11\n[cursor]\nblink=' + str(options.blink).lower() + '\n')
        with log_path.open("w") as log:
            process = subprocess.Popen(
                [str(options.binary), "--config", str(config), "-e", "python3", "-u", str(ROOT / "tests/fixtures/gui_scene.py"), "echo"],
                env=os.environ | {"WISP_TRACE_TIMING": "1"},
                stdout=log,
                stderr=log,
            )
            try:
                client = wait_for(lambda: next((client for client in clients() if client["pid"] == process.pid and client["title"] == "wisp-gui-ready"), None), "latency window")
                dispatch("focuswindow", "address:" + client["address"])
                time.sleep(0.5)
                for idle in options.idle:
                    time.sleep(idle)
                    focused = json.loads(run("hyprctl", "activewindow", "-j").stdout)
                    if focused.get("pid") != process.pid:
                        dispatch("focuswindow", "address:" + client["address"])
                        time.sleep(0.1)
                    offset = log_path.stat().st_size
                    run("wtype", "x")
                    time.sleep(0.25)
                    events = []
                    with log_path.open() as captured:
                        captured.seek(offset)
                        for line in captured:
                            if line.startswith("wisp-timing\t"):
                                _, timestamp, stage = line.strip().split("\t")
                                events.append((int(timestamp), stage))
                    start = next(timestamp for timestamp, stage in events if stage == "keyboard-event")
                    def elapsed(stage):
                        return (next(timestamp for timestamp, name in events if name == stage and timestamp >= start) - start) / 1000
                    output = next(timestamp for timestamp, stage in events if stage == "pty-output" and timestamp >= start)
                    snapshot = next(timestamp for timestamp, stage in events if stage == "snapshot-publish" and timestamp >= output)
                    layout = next(timestamp for timestamp, stage in events if stage == "snapshot-layout-end" and timestamp >= snapshot)
                    present = next(timestamp for timestamp, stage in events if stage == "frame-presented" and timestamp >= layout)
                    print(f"idle={idle:g}s event->PTY={(output-start)/1000:.3f}ms event->echo-snapshot={(snapshot-start)/1000:.3f}ms event->echo-present={(present-start)/1000:.3f}ms", flush=True)
                    acquire = next(timestamp for timestamp, stage in events if stage == "surface-acquire-start" and timestamp >= start)
                    complete = next(timestamp for timestamp, stage in events if stage == "surface-acquire-end" and timestamp >= acquire)
                    print(f"  acquire={(complete - acquire) / 1000:.3f}ms", flush=True)
            finally:
                process.terminate()
                process.wait(timeout=5)
    if active:
        dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
