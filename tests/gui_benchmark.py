import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time

from gui_smoke import ROOT, dispatch, find_client, run, wait_for


PROBE = r"""
import os
from pathlib import Path
import subprocess
import sys
import time

directory = Path(sys.argv[1])
os.write(1, b'\x1b]2;wisp-benchmark-ready\x1b\\')
while not (directory / 'start').exists():
    time.sleep(0.02)
size = os.get_terminal_size(0)
(directory / 'terminal-size').write_text(f'{size.columns}x{size.lines}')
with (directory / 'report').open('wb') as report:
    status = subprocess.run(['kitten', '__benchmark__', *sys.argv[2:]], stdout=report, stderr=report).returncode
(directory / 'done').write_text(str(status))
os.write(1, b'\x1b]2;wisp-benchmark-done\x1b\\')
while True:
    time.sleep(0.2)
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/wisp")
    parser.add_argument("--config", type=Path, default=Path.home() / ".config/wisp/config.toml")
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--render", action="store_true")
    parser.add_argument("--with-scrollback", action="store_true")
    parser.add_argument("--width", type=int, default=1600)
    parser.add_argument("--height", type=int, default=900)
    parser.add_argument("--label", default="current")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "target/gui-benchmark")
    parser.add_argument("benchmarks", nargs="*", default=["unique_unicode", "csi", "long_escape_codes"])
    options = parser.parse_args()
    options.artifacts.mkdir(parents=True, exist_ok=True)
    active = json.loads(run("hyprctl", "activewindow", "-j").stdout).get("address")
    arguments = ["--repetitions", str(options.repetitions)]
    if options.render:
        arguments.append("--render")
    if options.with_scrollback:
        arguments.append("--with-scrollback")
    arguments.extend(options.benchmarks)
    log_path = options.artifacts / f"{options.label}.log"
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="wisp-gui-benchmark-") as temporary:
            directory = Path(temporary)
            command = [str(options.binary.resolve()), "--config", str(options.config.resolve()), "-e", "python3", "-u", "-c", PROBE, str(directory), *arguments]
            with log_path.open("w") as log:
                process = subprocess.Popen(command, stdout=log, stderr=log)
                client = wait_for(lambda: find_client(process.pid), "mapped benchmark window")
                address = "address:" + client["address"]
                if not client["floating"]:
                    dispatch("togglefloating", address)
                dispatch("resizewindowpixel", f"exact {options.width} {options.height}," + address)
                dispatch("movewindowpixel", "exact 100 100," + address)
                wait_for(lambda: (find_client(process.pid) or {}).get("title") == "wisp-benchmark-ready", "benchmark ready")
                time.sleep(0.5)
                client = find_client(process.pid)
                assert client["size"] == [options.width, options.height], client["size"]
                (directory / "start").touch()
                wait_for(lambda: (directory / "done").exists(), "benchmark completion", timeout=180)
                assert (directory / "done").read_text() == "0", (directory / "report").read_text()
                report = re.sub(r"\x1b\[[0-9;]*m", "", (directory / "report").read_text()).replace("\x1b]\x1b\\\x1bc", "")
                result = {
                    "binary": str(options.binary.resolve()),
                    "config": str(options.config.resolve()),
                    "window_size": client["size"],
                    "terminal_size": (directory / "terminal-size").read_text(),
                    "arguments": arguments,
                    "report": report,
                }
                output = options.artifacts / f"{options.label}.json"
                output.write_text(json.dumps(result, indent=2) + "\n")
                print(json.dumps(result, indent=2))
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        if active:
            dispatch("focuswindow", "address:" + active)


if __name__ == "__main__":
    main()
