#!/usr/bin/env python3
"""Record actual means TUI output in a PTY against a new synthetic ledger."""
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "website/public/media"
COLS, ROWS = 160, 42
SCENES = {
    "overview": [(2, "2"), (3, "j"), (3.6, "\r"), (5.2, "z"), (6.7, "\x1b"),
                 (7.6, "\r"), (10, "\x1b"), (10.6, "\x1b"), (11.2, "7"),
                 (12.2, "c"), (15.2, "1"), (16.5, "e"), (19.5, "e"), (22, "q")],
    "review-split": [(1.5, "4"), (3.5, "S"), (5, "a"), (6, "Groceries"),
                     (7.2, "\r"), (8.3, "72.40"), (9.5, "\r"), (11, "a"),
                     (12, "Household"), (13.3, "\r"), (14.6, "\r"),
                     (17, "\r"), (21, "y"), (23, "3"), (24.5, "\r"),
                     (28, "\x1b"), (29, "q")],
}

def record(name, actions):
    with tempfile.TemporaryDirectory(prefix="means-tui-demo-") as temp:
        work = Path(temp)
        db = work / "demo.db"
        subprocess.run([str(ROOT / "target/debug/examples/website_demo"), str(db)], check=True)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        # Provider credentials are deliberately absent from this environment.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color", "LANG": "en_US.UTF-8"}
        server = subprocess.Popen([str(ROOT / "target/debug/means"), "--db", str(db), "serve", "--listen", f"127.0.0.1:{port}", "--inbox", str(work / "inbox")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        terminal = None
        master = None
        try:
            deadline = time.monotonic() + 15
            while True:
                try:
                    with socket.create_connection(("127.0.0.1", port), .2):
                        break
                except OSError:
                    if time.monotonic() > deadline or server.poll() is not None:
                        raise RuntimeError("Synthetic demo server did not start")
                    time.sleep(.1)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
            terminal = subprocess.Popen([str(ROOT / "target/debug/means"), "tui", "--server", f"http://127.0.0.1:{port}"], stdin=slave, stdout=slave, stderr=slave, env=env)
            os.close(slave)
            start = time.monotonic()
            events = []
            decoder = codecs.getincrementaldecoder("utf-8")("replace")
            pending = list(actions)
            while time.monotonic() - start < actions[-1][0] + 2:
                elapsed = time.monotonic() - start
                while pending and elapsed >= pending[0][0]:
                    _, keys = pending.pop(0)
                    os.write(master, keys.encode())
                if select.select([master], [], [], .025)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    events.append([round(elapsed, 3), "o", decoder.decode(data)])
                if terminal.poll() is not None:
                    break
            header = {"version": 2, "width": COLS, "height": ROWS, "title": f"means / {name} / synthetic data", "env": {"TERM": "xterm-256color"}}
            OUTPUT.mkdir(parents=True, exist_ok=True)
            (OUTPUT / f"{name}.cast").write_text("\n".join(json.dumps(event) for event in [header, *events]) + "\n")
            if terminal.poll() is None:
                raise RuntimeError(f"{name}: TUI did not exit after the recorded workflow")
            if terminal.returncode:
                raise RuntimeError(f"{name}: TUI exited with {terminal.returncode}")
            print(f"Recorded {name}: {len(events)} terminal output events")
        finally:
            if terminal is not None and terminal.poll() is None:
                terminal.terminate()
                terminal.wait(timeout=5)
            if master is not None:
                os.close(master)
            server.terminate()
            server.wait(timeout=5)

if __name__ == "__main__":
    for name, actions in SCENES.items():
        record(name, actions)
