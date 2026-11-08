import os
import sys
import tempfile


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


with tempfile.TemporaryDirectory(prefix="wisp-nano-") as directory:
    path = os.path.join(directory, "mouse-target.txt")
    with open(path, "w", encoding="utf-8") as output:
        output.write("alpha bravo charlie delta echo foxtrot\n")
        output.write("second line with a visible mouse destination\n")
        output.write("third line with more text to select\n")
    pid = os.fork()
    if pid == 0:
        os.execvp("nano", ["nano", "--ignorercfiles", "--mouse", path])
    _, status = os.waitpid(pid, 0)
    title("nano-status:" + str(os.waitstatus_to_exitcode(status)))
