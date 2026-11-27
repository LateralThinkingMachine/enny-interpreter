"""Exercise the extracted Linux download through its real interactive BYOK prompts."""
import argparse
from contextlib import closing
import http.client
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import sys
import time
import urllib.request


def check_quote(captured, minutes, cents):
    credential_line = re.search(rb"Select this file in the browser within two minutes: ([^\r\n]+)", captured)
    if not credential_line:
        raise AssertionError("The operator credential path was not provided")
    credentials = json.loads(Path(os.fsdecode(credential_line.group(1))).read_text())
    headers = {"Origin": "http://127.0.0.1:5180", "Content-Type": "application/json"}
    with closing(http.client.HTTPConnection("127.0.0.1", 5180, timeout=3)) as client:
        client.request("POST", "/api/bootstrap", json.dumps({"ticket": credentials["bootstrap"]}), headers)
        response = client.getresponse()
        assert response.status == 200, "The operator could not sign in"
        session = response.headers["X-Pilot-Session"]
        response.read()
        client.request("GET", "/api/budget", headers={"Authorization": "Bearer " + session})
        response = client.getresponse()
        assert response.status == 200, "The operator could not read the estimate"
        budget = json.load(response)
    assert budget["route_duration_seconds"] == minutes * 60
    assert budget["amounts"]["approved_microusd"] == cents * 10_000
    assert budget["stream_reservations"]["used"] == 0
    assert budget["amounts"]["reserved_microusd"] == 0
    routes = {route["route"]: route for route in budget["routes"] if route["configured"]}
    assert set(routes) == {"live", "transcription"}
    assert all(route["can_reserve"] and route["quote_seconds"] == minutes * 60 for route in routes.values())
    assert sum(route["quote_microusd"] for route in routes.values()) == minutes * 67_000


def exercise(root, minutes, dollars, cents, expected_estimate):
    child, fd = pty.fork()
    if child == 0:
        os.chdir(root)
        os.execv(sys.executable, [sys.executable, "start.py", "--live"])
    captured = bytearray()
    marker = 0

    def wait_for(prompt, timeout=20):
        nonlocal marker
        limit = time.monotonic() + timeout
        while (end := captured.find(prompt, marker)) < 0:
            remaining = limit - time.monotonic()
            if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
                raise RuntimeError("Packaged launcher did not reach the next prompt")
            captured.extend(os.read(fd, 8192))
        marker = end + len(prompt)

    dummy = b"private-inert-credential-for-packaged-test"
    try:
        wait_for(b"[60]: ")
        os.write(fd, (str(minutes) + "\n").encode())
        wait_for(b"[5.00]: ")
        os.write(fd, b"0.01\n")
        wait_for(b"[5.00]: ")
        os.write(fd, (dollars + "\n").encode())
        for _ in range(4):
            wait_for(b"[y/N]")
            os.write(fd, b"yes\n")
        wait_for(f"Estimated OpenAI cost for {minutes} minutes on this computer: US${expected_estimate}.".encode())
        wait_for(f"Your local estimate limit: US${dollars}.".encode())
        wait_for(b"OpenAI charges can begin when you start live processing in the browser")
        wait_for("Start live interpretation · billed by your own OpenAI account [y/N]".encode())
        os.write(fd, b"yes\n")
        wait_for(b"(hidden): ")
        os.write(fd, dummy + b"\n")
        wait_for(b"Press Ctrl-C to stop.")
        with urllib.request.urlopen("http://127.0.0.1:5180/api/health", timeout=2) as health:
            assert json.load(health)["providers_enabled"] is True
        check_quote(captured, minutes, cents)
        assert dummy not in captured, "A credential was displayed in the terminal"
        os.write(fd, b"\x03")
        wait_for(b"Stopping the interpreter.")
        limit = time.monotonic() + 40
        while time.monotonic() < limit:
            done, status = os.waitpid(child, os.WNOHANG)
            if done:
                child = None
                if os.waitstatus_to_exitcode(status) != 0:
                    raise RuntimeError("Packaged launcher reported an unclean shutdown")
                print(f"Packaged {minutes}-minute approval, budget quote, hidden key and Ctrl-C shutdown passed.")
                return
            time.sleep(0.05)
        raise RuntimeError("Packaged launcher did not close")
    finally:
        os.close(fd)
        if child is not None:
            try:
                os.kill(child, signal.SIGTERM)
            except ProcessLookupError:
                pass
            os.waitpid(child, 0)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("extracted", type=Path)
    extracted = parser.parse_args().extracted.resolve(strict=True)
    exercise(extracted, 60, "5.00", 500, "4.02")
    exercise(extracted, 17, "1.20", 120, "1.14")
