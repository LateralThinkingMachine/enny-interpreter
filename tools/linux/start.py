#!/usr/bin/env python3
"""Start free rehearsal, or use --live for interpretation billed by your own OpenAI account."""
import argparse
import getpass
import json
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import tempfile
import warnings


ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "tools"))
from local_call_pilot import ORIGIN, browser_entry, wait_ready


DEFAULT_MINUTES = 60
DEFAULT_BUDGET_CENTS = 500


def usd(cents):
    return f"{cents // 100}.{cents % 100:02}"


def estimated_cents(minutes):
    # Captions and interpretation together cost an estimated 6.7 cents per minute.
    return (minutes * 67 + 9) // 10


def session_limits():
    while True:
        value = input(f"Meeting length in minutes (1–60) [{DEFAULT_MINUTES}]: ").strip()
        if value == "":
            minutes = DEFAULT_MINUTES
            break
        if re.fullmatch(r"[0-9]{1,2}", value) and 1 <= int(value) <= 60:
            minutes = int(value)
            break
        print("Enter a whole number from 1 to 60.")

    minimum_cents = estimated_cents(minutes)
    print(f"Estimated OpenAI cost for {minutes} minutes on this computer: US${usd(minimum_cents)} (rounded up to cents).")
    while True:
        value = input(f"Cost limit in US dollars [{usd(DEFAULT_BUDGET_CENTS)}]: ").strip() or usd(DEFAULT_BUDGET_CENTS)
        if re.fullmatch(r"[0-9]{1,4}(\.[0-9]{1,2})?", value):
            whole, _, fraction = value.partition(".")
            cents = int(whole) * 100 + int(fraction.ljust(2, "0"))
            if minimum_cents <= cents <= 100_000:
                return minutes, cents
        print(f"Enter US${usd(minimum_cents)} to US$1000.00 with up to two decimal places.")
        print("To choose a shorter meeting, press Ctrl+C and start again.")


def approve_session():
    if not sys.stdin.isatty():
        raise ValueError("Live approval requires an interactive terminal.")
    print("Enny sends the selected audio to OpenAI for captions and spoken interpretation.")
    print("Enny charges no fee. OpenAI bills the account that owns your API key.")
    minutes, cents = session_limits()
    for statement in (
        "All participants agree to this processing.",
        "I have checked which audio source I will share.",
        "I have checked private headphone playback.",
        "I understand that interpretation can be inaccurate.",
    ):
        if input(f"{statement} [y/N] ").strip().lower() not in {"y", "yes"}:
            raise ValueError("Live session was not approved.")
    print(f"Estimated OpenAI cost for {minutes} minutes on this computer: US${usd(estimated_cents(minutes))}.")
    print(f"Your local estimate limit: US${usd(cents)}. OpenAI determines the actual charge.")
    print("Confirming here opens the local operator page after key entry. Opening that page is free.")
    print("OpenAI charges can begin when you start live processing in the browser and provider connections open.")
    if input("Start live interpretation · billed by your own OpenAI account [y/N] ").strip().lower() not in {"y", "yes"}:
        raise ValueError("Live session was not approved.")
    with warnings.catch_warnings():
        warnings.simplefilter("error", getpass.GetPassWarning)
        try:
            key = getpass.getpass("OpenAI key for this session (hidden): ")
        except getpass.GetPassWarning:
            raise ValueError("This terminal cannot conceal the key.") from None
    if not 16 <= len(key) <= 4096 or any(not 33 <= ord(char) <= 126 for char in key):
        raise ValueError("The key must contain 16–4096 printable ASCII characters without spaces.")
    return minutes, cents, key


def run(approval, check):
    with tempfile.TemporaryDirectory(prefix="meeting-interpreter-") as temp:
        directory = Path(temp).resolve()
        config = {
            "assets_dir": str(ROOT / "web"), "credentials_path": str(directory / "credentials.json"),
            "operator_port": 5180,
        }
        if approval is None:
            timeout = 600 + 40
            config.update(duration_seconds=600, max_input_audio_seconds=1200, provider=None)
        else:
            minutes, cents, key = approval
            # Allow time to select audio and time for native service shutdown.
            timeout = minutes * 60 + 120 + 40
            config.update(meeting_minutes=minutes, budget_cents=cents, api_key=key)
        frame = json.dumps(config).encode()
        if len(frame) > 16 * 1024:
            raise ValueError("Session configuration is too large.")
        with subprocess.Popen(
            [ROOT / "bin/meeting-call-pilot", "--desktop-pipe"], stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=directory,
            env={"PATH": "/usr/bin:/bin", "LANG": "C"}, start_new_session=True,
        ) as child:
            try:
                child.stdin.write(struct.pack(">I", len(frame)) + frame)
                child.stdin.flush()
                wait_ready(child, directory, providers_enabled=approval is not None)
                if check:
                    print("Packaged session configuration accepted.")
                else:
                    browser_entry(directory, open_browser=True, origin=ORIGIN)
                    print("Press Ctrl-C to stop. Keep the operator tab private.", flush=True)
                    child.wait(timeout=timeout)
            except KeyboardInterrupt:
                print("Stopping the interpreter.", flush=True)
            finally:
                try:
                    child.stdin.close()
                except BrokenPipeError:
                    pass
                try:
                    child.wait(timeout=38)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                    raise RuntimeError("Local service was terminated. Check provider usage for any approved live session.") from None
                if child.returncode:
                    if approval is not None:
                        raise RuntimeError("Live session closure is uncertain. Check active sessions and billing with your provider.")
                    raise RuntimeError("The local rehearsal service exited unexpectedly.")


def request_stop(_number, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--live", action="store_true", help="Start live interpretation · billed by your own OpenAI account")
    mode.add_argument("--check", action="store_true", help="verify key-free startup and shutdown")
    mode.add_argument("--check-live-config", action="store_true", help="verify the one-hour live configuration with an inert test value")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, request_stop)
    signal.signal(signal.SIGHUP, request_stop)
    if not any(vars(args).values()):
        print("Start free rehearsal")
        print("Estimated cost: US$0.00. Enny keeps the audio on this computer. No provider key is needed.")
        print("For live interpretation billed by OpenAI, run python3 start.py --live.")
    approval = None
    if args.live:
        approval = approve_session()
    elif args.check_live_config:
        approval = (DEFAULT_MINUTES, DEFAULT_BUDGET_CENTS, "fictional-key-never-transmitted")
    run(approval, args.check or args.check_live_config)


if __name__ == "__main__":
    try:
        main()
    except (EOFError, KeyboardInterrupt):
        sys.exit("Session canceled.")
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        sys.exit(f"Interpreter: {error}")
