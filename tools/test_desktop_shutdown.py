"""Qualify an extracted desktop package with inert credentials and no provider.

Run on the package's native OS with --expiry to also wait for the real idle
deadline. The outcome matrix uses the packaged launcher byte-for-byte in a
separate temporary layout. Only its helper is replaced by a fixture there.
"""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import pty
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time


EXPECTED = {
    20: "Local audio stopped. Enny did not start any OpenAI work.",
    21: "Local audio stopped. OpenAI confirmed that all admitted connections closed.",
    22: "Local audio stopped. OpenAI closure is unconfirmed. Check usage in your OpenAI account.",
}


def synthetic_helper():
    # This copy runs exclusively in the temporary fixture layout.
    case = Path(__file__).with_name("case.json")
    code, ending = json.loads(case.read_text())
    signal.alarm(70 if ending == "hung" else 15)
    wire = sys.stdin.buffer
    size = int.from_bytes(wire.read(4), "big")
    config = json.loads(wire.read(size))
    assert config["api_key"] == "fictional-key-never-transmitted"
    path = Path(config["credentials_path"])
    probed = threading.Event()

    class Health(BaseHTTPRequestHandler):
        def do_GET(self):
            assert self.path == "/api/health"
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"mode":"supervised_local_pilot","providers_enabled":true}')
            probed.set()

        def log_message(self, *_args):
            pass

    if ending == "signal-setup":
        case.with_suffix(".starting").touch()
    else:
        if sys.platform.startswith("linux"):
            server = HTTPServer(("127.0.0.1", config["operator_port"]), Health)
            threading.Thread(target=server.serve_forever, daemon=True).start()
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(fd, "w") as stream:
            json.dump({"origin": "http://127.0.0.1:5180", "bootstrap": "b" * 64}, stream)
    if ending in ("stop", "hung", "signal-setup", "signal-active"):
        assert wire.read(1) == b"", "Expected parent pipe to close"
        if ending == "hung":
            time.sleep(65)
        if ending.startswith("signal-"):
            case.with_suffix(".closing").touch()
            while not case.with_suffix(".release").exists():
                time.sleep(0.01)
    else:
        if sys.platform == "darwin":
            # Session.entry consumes the private credential file before waiting.
            while path.exists():
                time.sleep(0.01)
        else:
            assert probed.wait(5)
        assert not select.select([wire], [], [], 0.15)[0], "Expected parent pipe to stay open"
    return code


def repeated_signals(root, case, during_setup):
    """Send real Stop signals to --live while its child delays retirement."""
    for ext in (".starting", ".closing", ".release"):
        case.with_suffix(ext).unlink(missing_ok=True)
    case.write_text(json.dumps([22, "signal-setup" if during_setup else "signal-active"]))
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.execv(sys.executable, [sys.executable, "-I", "start.py", "--live"])
    output = bytearray()

    def until(prompt):
        deadline = time.monotonic() + 10
        while prompt not in output:
            elapsed = deadline - time.monotonic()
            if elapsed <= 0 or not select.select([terminal], [], [], elapsed)[0]:
                raise AssertionError(f"Missing terminal prompt {prompt!r}")
            output.extend(os.read(terminal, 8192))

    def marker(ext):
        deadline = time.monotonic() + 5
        while not case.with_suffix(ext).exists():
            if time.monotonic() >= deadline:
                raise AssertionError(f"Child missed {ext}")
            time.sleep(0.01)

    try:
        until(b"[60]: ")
        # In a terminal, input can be queued for the four confirmations and
        # final approval. Wait for the concealed key prompt before writing it.
        os.write(terminal, b"1\n0.07\ny\ny\ny\ny\ny\n")
        until(b"(hidden): ")
        os.write(terminal, b"fictional-key-never-transmitted\n")
        if during_setup:
            marker(".starting")
        else:
            until(b"Press Ctrl-C to stop.")
        os.kill(pid, signal.SIGINT)
        marker(".closing")
        for number in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            os.kill(pid, number)
            time.sleep(0.03)
        case.with_suffix(".release").touch()
        until(EXPECTED[22].encode())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                pid = None
                assert os.waitstatus_to_exitcode(status) == 1
                break
            time.sleep(0.01)
        assert pid is None, "Launcher did not finish after the receipt"
        # Read any remaining lines, including a duplicated terminal message.
        while select.select([terminal], [], [], 0)[0]:
            try:
                chunk = os.read(terminal, 8192)
            except OSError:
                break
            if not chunk:
                break
            output.extend(chunk)
        assert output.splitlines().count(EXPECTED[22].encode()) == 1
        assert b"Session canceled." not in output and b"Traceback" not in output
    finally:
        os.close(terminal)
        if pid is not None:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


def check(package, full_expiry):
    mac = sys.platform == "darwin"
    launcher = Path("Contents/MacOS/Interpreter" if mac else "start.py")
    helper = Path("Contents/Helpers/meeting-call-pilot" if mac else "bin/meeting-call-pilot")

    def run(root, ending, code, timeout):
        command = [str(root / launcher)] if mac else [sys.executable, "-I", str(root / launcher)]
        arguments = ["--check-idle-expiry"] if ending == "expiry" else ["--check-live-config"]
        if mac and ending == "stop":
            arguments += ["1", "0.07"]
        result = subprocess.run(command + arguments, capture_output=True, text=True, timeout=timeout)
        text = result.stdout + result.stderr
        expected = EXPECTED.get(code, EXPECTED[22])
        # The Mac diagnostic also expects zero admissions when it supplies an inert key.
        success = code == 20 if mac else code in (20, 21)
        assert (result.returncode == 0) == success, (ending, code, result.returncode, text)
        assert text.splitlines().count(expected) == 1, (ending, code, text)
        assert all(other not in text for other in EXPECTED.values() if other != expected)

    run(package, "stop", 20, 55)
    print("Original package: parent-pipe Stop, no provider work admitted.", flush=True)
    if full_expiry:
        started = time.monotonic()
        run(package, "expiry", 20, 235)
        elapsed = time.monotonic() - started
        assert elapsed >= 179, "The native one-minute window plus bootstrap grace did not elapse"
        print(f"Original package: natural idle expiry, no provider work admitted ({elapsed:.1f}s).", flush=True)

    with tempfile.TemporaryDirectory(prefix="enny-launcher-receipts-") as temp:
        root = Path(temp) / package.name
        shutil.copytree(package, root)
        def digest(path):
            return hashlib.sha256(path.read_bytes()).digest()
        assert digest(root / launcher) == digest(package / launcher)
        fixture = root / helper
        fixture.write_bytes(b"#!" + os.fsencode(sys.executable) + b"\n" + Path(__file__).read_bytes())
        fixture.chmod(0o755)
        for ending in ("stop", "expiry"):
            for code in (20, 21, 22, 0):
                fixture.with_name("case.json").write_text(json.dumps([code, ending]))
                run(root, ending, code, 25)
        if full_expiry:
            # The child would return a favorable code if left running. Both
            # launchers must bound their wait, reap it and report uncertainty.
            fixture.with_name("case.json").write_text(json.dumps([21, "hung"]))
            started = time.monotonic()
            run(root, "stop", 22, 55)
            elapsed = time.monotonic() - started
            assert 37 <= elapsed < 55, "Hung helper escaped its retirement bounds"
            print(f"Packaged launcher: a hung synthetic helper gave an uncertain receipt after {elapsed:.1f}s.")
        if not mac:
            for during_setup in (True, False):
                repeated_signals(root, fixture.with_name("case.json"), during_setup)
            print("Linux interactive launcher: repeated Stop signals during setup and active use preserved one uncertain receipt.")
        assert digest(root / launcher) == digest(package / launcher)
    print("Packaged launcher with synthetic child: Stop and expiry showed all three outcomes; status zero stayed unconfirmed.")


if __name__ == "__main__":
    if sys.argv[1:] == ["--desktop-pipe"]:
        sys.exit(synthetic_helper())
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path, help="extracted .app on macOS or directory containing start.py on Linux")
    parser.add_argument("--expiry", action="store_true", help="also wait about three minutes for the actual native deadline")
    opts = parser.parse_args()
    check(opts.package.resolve(strict=True), opts.expiry)
