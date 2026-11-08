import base64
import os
from pathlib import Path
import select
import subprocess
import struct
import sys
import tempfile
import termios
import tty
import zlib


RED = bytes((255, 0, 0, 255))
GREEN = bytes((0, 255, 0, 255))
BLUE = bytes((0, 0, 255, 255))
CLEAR = bytes(4)


def command(controls, data=b""):
    return b"\x1b_G" + controls.encode() + b";" + base64.b64encode(data) + b"\x1b\\"


def title(value):
    os.write(1, b"\x1b]2;" + value.encode() + b"\x1b\\")


def write_all(data):
    while data:
        data = data[os.write(1, data):]


def png(width, height, rgba):
    def block(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + rgba[offset:offset + width * 4]
                    for offset in range(0, len(rgba), width * 4))
    return (b"\x89PNG\r\n\x1a\n"
            + block(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + block(b"IDAT", zlib.compress(rows)) + block(b"IEND", b""))


def read_reply():
    reply = b""
    while not reply.endswith(b"\x1b[0n"):
        if not select.select([0], [], [], 5)[0]:
            raise RuntimeError("terminal query timed out")
        reply += os.read(0, 65536)
    return reply[:-4]


def stage(name, *data):
    if os.read(0, 1) != b"g":
        raise SystemExit
    for part in data:
        write_all(part)
    write_all(b"\x1b[5n")
    reply = read_reply()
    title("graphics-stage:" + name + ":" + reply.hex())


def scenarios(scenario):
    if scenario == "responses":
        stage("query", command("a=q,i=11,f=24,s=1,v=1", bytes(3)))
        stage("quiet", command("a=t,i=12,f=32,s=1,v=1,q=1", RED),
              command("a=p,i=999,p=2,q=1"), command("a=p,i=998,q=2"))
        stage("numbers", command("a=t,I=901,f=32,s=1,v=1", RED),
              command("a=t,I=901,f=32,s=1,v=1", GREEN),
              command("a=p,I=901,p=3,C=1"))
    elif scenario == "features":
        stage("partial", command("a=T,i=21,f=32,s=2,v=1,C=1,m=1", RED))
        stage("features", command("a=q,i=991,e=T,U=1,C=1"),
              command("a=q,i=992,e=f,r=1,X=1"),
              command("a=q,i=993,e=a,s=3,v=2"),
              command("a=q,i=994,e=c,C=1,w=1"))
        stage("complete", command("m=0", GREEN))
    elif scenario == "indexed":
        stage("image", command("a=T,i=71,f=8,s=3,v=1,C=1", bytes((0, 248, 249))))
        stage("palette", b"\x1b]4;0;#123456\x1b\\\x1b]10;#abcdef\x1b\\\x1b]11;#112233\x1b\\")
        stage("alpha", command("a=T,i=72,f=16,s=2,v=1,C=1", bytes((0, 128, 249, 0))))
        stage("reject-animation", command("a=f,i=71,f=32,s=1,v=1", RED))
    elif scenario == "range":
        stage("images", command("a=T,i=10,p=1,f=32,s=1,v=1,C=1", RED),
              command("a=T,i=20,p=1,f=32,s=1,v=1,C=1", GREEN),
              command("a=T,i=30,p=1,f=32,s=1,v=1,C=1", BLUE))
        stage("delete", command("a=d,d=R,x=20,y=20"))
        stage("reload", command("a=p,i=20,C=1"))
    elif scenario == "lifecycle":
        stage("explicit", command("a=T,i=2147483647,p=1,f=32,s=1,v=1,C=1", RED))
        stage("automatic", command("a=T,I=912,p=1,f=32,s=1,v=1,C=1", GREEN))
        stage("partial", command("a=T,i=22,f=32,s=2,v=1,m=1,C=1", RED))
        stage("abort", command("a=d,d=i,i=22"),
              command("a=T,i=23,f=32,s=1,v=1,C=1", BLUE))
    elif scenario == "replacement":
        stage("root", command("a=T,i=24,p=1,f=32,s=1,v=1,C=1", RED))
        stage("replace", command("a=t,i=24,f=32,s=1,v=1", GREEN))
        stage("place", command("a=p,i=24,p=2,C=1"))
        stage("bad-replace", command("a=t,i=24,f=32,s=1,v=1", bytes(3)))
    elif scenario == "file-errors":
        with tempfile.TemporaryDirectory(prefix="wisp-graphics-file-") as temporary:
            directory = Path(temporary)
            regular = directory / "image.raw"
            regular.write_bytes(b"prefix" + RED + b"suffix")
            empty = directory / "empty.raw"
            empty.touch()
            fifo = directory / "fifo"
            os.mkfifo(fifo)
            stage("file", command("a=T,i=25,f=32,s=1,v=1,t=f,O=6,S=4,C=1", os.fsencode(regular)))
            stage("errors", command("a=q,i=26,f=32,s=1,v=1,t=f", os.fsencode(directory / "missing")),
                  command("a=q,i=27,f=32,s=1,v=1,t=f", os.fsencode(empty)),
                  command("a=q,i=28,f=32,s=1,v=1,t=f", os.fsencode(directory)),
                  command("a=q,i=29,f=32,s=1,v=1,t=f", b"/dev/null"),
                  command("a=q,i=30,f=32,s=1,v=1,t=f", os.fsencode(fifo)),
                  command("a=f,i=25,f=32,s=1,v=1,t=f", os.fsencode(empty)),
                  command("a=T,i=31,f=8,s=1,v=1,t=f,C=1", os.fsencode(empty)))
    elif scenario == "animation":
        stage("root", command("a=T,i=61,p=1,f=32,s=2,v=2,C=1", RED * 4))
        stage("frame", command("a=f,i=61,f=32,s=1,v=1,x=1,y=1,c=1,z=75", GREEN))
        stage("select", command("a=a,i=61,c=2"))
        stage("edit-blend", command("a=f,i=61,f=32,s=1,v=1,x=0,y=0,r=2", CLEAR))
        stage("edit-overwrite", command("a=f,i=61,f=32,s=1,v=1,x=0,y=0,r=2,X=1", CLEAR))
        stage("compose", command("a=c,i=61,r=1,c=2,w=1,h=1,x=1,y=1,C=1"))
        stage("compose-errors", command("a=c,i=61,r=2,c=2,w=1,h=1"),
              command("a=c,i=61,r=1,c=2,x=4294967295"),
              command("a=c,i=61,r=99,c=2,w=1,h=1"))
        stage("delete-root", command("a=d,d=f,i=61,r=1"))
        stage("delete-last", command("a=d,d=F,i=61,r=1"))
    elif scenario == "animation-chunks":
        stage("root", command("a=T,i=62,p=1,f=32,s=2,v=1,C=1", RED * 2))
        stage("partial", command("a=f,i=62,f=32,s=2,v=1,m=1,z=77", GREEN),
              command("m=1", BLUE[:3]))
        stage("complete", command("m=0", BLUE[3:]))
        stage("select", command("a=a,i=62,c=2"))
        stage("compressed", command("a=f,i=62,f=32,s=2,v=1,o=z,m=1", zlib.compress(BLUE * 2)[:4]),
              command("a=f,m=0", zlib.compress(BLUE * 2)[4:]))
        stage("select-third", command("a=a,i=62,c=3"))
        stage("quiet", command("a=f,i=62,f=32,s=2,v=1,q=1,m=1", RED), command("a=f,m=0", RED))
        stage("repeat-id", command("a=f,i=62,f=32,s=2,v=1,m=1", GREEN),
              command("a=f,i=62,m=0", GREEN))
        stage("repeat-id-select", command("a=a,i=62,c=5"))
    elif scenario == "animation-transport":
        with tempfile.TemporaryDirectory(prefix="wisp-graphics-transport-") as temporary:
            directory = Path(temporary)
            source = directory / "normal.raw"
            source.write_bytes(b"prefix" + GREEN + BLUE + b"suffix")
            transient = directory / "tty-graphics-protocol-frame.raw"
            transient.write_bytes(BLUE * 2)
            shared_name = "wisp-graphics-frame-" + str(os.getpid())
            shared = Path("/dev/shm") / shared_name
            shared.write_bytes(RED + GREEN)
            try:
                stage("root", command("a=T,I=913,p=1,f=32,s=2,v=1,C=1", RED * 2))
                stage("file", command("a=f,I=913,t=f,f=32,s=2,v=1,O=6,S=8", os.fsencode(source)))
                assert source.exists()
                stage("file-select", command("a=a,I=913,c=2"))
                stage("temporary", command("a=f,I=913,t=t,f=32,s=2,v=1", os.fsencode(transient)))
                assert not transient.exists()
                stage("temporary-select", command("a=a,I=913,c=3"))
                stage("shared", command("a=f,I=913,t=s,f=32,s=2,v=1", ("/" + shared_name).encode()))
                assert not shared.exists()
                stage("shared-select", command("a=a,I=913,c=4"))
                stage("png", command("a=f,I=913,f=100", png(2, 1, GREEN + RED)))
                stage("png-select", command("a=a,I=913,c=5"))
            finally:
                shared.unlink(missing_ok=True)
    elif scenario == "animation-canvas":
        stage("root", command("a=T,i=65,p=1,f=32,s=2,v=2,C=1", RED * 4))
        stage("canvas", command("a=f,i=65,f=32,s=1,v=1,x=1,y=1,Y=65535", GREEN))
        stage("select", command("a=a,i=65,c=2"))
        stage("half-alpha", command("a=f,i=65,f=32,s=1,v=1,r=2", bytes((255, 0, 0, 128))))
        stage("errors", command("a=f,i=999,f=32,s=1,v=1", RED),
              command("a=f,i=65,I=913,f=32,s=1,v=1", RED),
              command("a=f,i=65,f=32,s=1,v=1,c=999", RED),
              b"\x1b_Ga=f,i=65,f=32,s=1,v=1;?invalid\x1b\\",
              command("a=f,i=65,t=x,f=32,s=1,v=1", RED))
    elif scenario == "animation-loading":
        stage("root", command("a=T,i=63,p=1,f=32,s=1,v=1,C=1", RED),
              command("a=f,i=63,f=32,s=1,v=1,z=100", GREEN))
        stage("loading", command("a=a,i=63,r=1,z=100,s=2"))
        stage("append", command("a=f,i=63,f=32,s=1,v=1,z=100", BLUE))
        stage("stop", command("a=a,i=63,s=1"))
        stage("finite", command("a=a,i=63,c=1,s=3,v=2"))
    elif scenario == "relative":
        stage("parent", b"\x1b[3;4H", command("a=T,i=101,p=1,f=32,s=1,v=1,c=2,r=1,C=1", RED))
        stage("child", command("a=T,i=102,p=2,f=32,s=1,v=1,c=1,r=1,P=101,Q=1,H=2,V=-1", GREEN))
        stage("move", b"\x1b[6;8H", command("a=p,i=101,p=1,c=2,r=1,C=1"))
        stage("errors", command("a=p,i=101,p=1,P=102,Q=2"),
              command("a=p,i=102,p=3,P=999,Q=1"))
        stage("delete", command("a=d,d=i,i=101,p=1"))
        stage("child-gone", command("a=p,i=102,C=1"))
    elif scenario == "screens":
        stage("main", command("a=T,i=64,p=1,f=32,s=1,v=1,C=1", RED),
              command("a=f,i=64,f=32,s=1,v=1", GREEN), command("a=a,i=64,c=2"))
        stage("alternate", b"\x1b[?1049h", command("a=T,i=64,p=1,f=32,s=1,v=1,C=1", BLUE))
        stage("restore", b"\x1b[?1049l")
        stage("reset", b"\x1bc")
    elif scenario == "icat":
        from PIL import Image

        directory = Path(sys.argv[2])
        image = directory / "animated.gif"
        frames = [Image.new("RGBA", (2, 2), color) for color in ("red", "green", "blue")]
        frames[0].save(image, save_all=True, append_images=frames[1:], duration=150, loop=0)
        if os.read(0, 1) != b"g":
            raise SystemExit
        result = subprocess.run([
            "kitten", "icat", "--engine=builtin", "--stdin=no", "--align=left",
            "--transfer-mode=stream", "--detection-timeout=1", "--loop=-1", str(image),
        ], check=False)
        title("graphics-stage:icat:" + str(result.returncode).encode().hex())
    else:
        raise ValueError(scenario)


original = termios.tcgetattr(0)
tty.setraw(0)
try:
    title("graphics-ready")
    scenarios(sys.argv[1])
    while b"\x04" not in os.read(0, 1024):
        pass
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
