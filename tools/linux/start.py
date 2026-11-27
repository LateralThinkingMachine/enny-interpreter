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
import time
import warnings


ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "tools"))
from local_call_pilot import ORIGIN, browser_entry, wait_ready


DEFAULT_MINUTES = 60
DEFAULT_BUDGET_CENTS = 500

# Positive receipts are reserved exit codes of the paid --desktop-pipe helper.
# An ordinary process exit, including status zero, gives no provider receipt.
NO_PROVIDER_WORK = 20
ALL_CONNECTIONS_CLOSED = 21
UNCERTAIN_CLOSURE = "Local audio stopped. OpenAI closure is unconfirmed. Check usage in your OpenAI account."


class UnconfirmedShutdown(RuntimeError):
    """The paid helper supplied no positive closure receipt."""


def write_status(message, *streams):
    for stream in streams:
        if stream is None:
            continue
        try:
            print(message, file=stream, flush=True)
            return
        except (OSError, ValueError):
            # A dead redirected stream must not mask the receipt or turn a
            # successful exit into a Python shutdown-time flush error.
            if stream is sys.stdout:
                sys.stdout = None
            if stream is sys.stderr:
                sys.stderr = None


def report_paid_shutdown(returncode):
    if returncode == NO_PROVIDER_WORK:
        write_status("Local audio stopped. Enny did not start any OpenAI work.", sys.stdout, sys.stderr)
    elif returncode == ALL_CONNECTIONS_CLOSED:
        write_status("Local audio stopped. OpenAI confirmed that all admitted connections closed.", sys.stdout, sys.stderr)
    else:
        raise UnconfirmedShutdown(UNCERTAIN_CLOSURE)



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


def wait_for_end(child, timeout, stop_requested):
    deadline = time.monotonic() + timeout
    while not stop_requested():
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(child.args, timeout)
        try:
            child.wait(timeout=min(0.2, remaining))
            return
        except subprocess.TimeoutExpired:
            continue


def run(approval, check, *, wait_for_expiry=False, stop_requested=lambda: False):
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
        if stop_requested():
            if approval is not None:
                report_paid_shutdown(NO_PROVIDER_WORK)
            return
        try:
            process = subprocess.Popen(
                [ROOT / "bin/meeting-call-pilot", "--desktop-pipe"], stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=directory,
                env={"PATH": "/usr/bin:/bin", "LANG": "C"}, start_new_session=True,
            )
        except OSError:
            if approval is not None:
                report_paid_shutdown(NO_PROVIDER_WORK)
            raise
        with process as child:
            try:
                child.stdin.write(struct.pack(">I", len(frame)) + frame)
                child.stdin.flush()
                wait_ready(child, directory, providers_enabled=approval is not None, stop_requested=stop_requested)
                if stop_requested():
                    return
                if check:
                    write_status("Packaged session configuration accepted.", sys.stdout)
                else:
                    browser_entry(directory, open_browser=True, origin=ORIGIN)
                    write_status("Press Ctrl-C to stop. Keep the operator tab private.", sys.stdout, sys.stderr)
                if not check or wait_for_expiry:
                    wait_for_end(child, timeout, stop_requested)
            finally:
                # Closing the lifetime pipe and making the bounded wait must precede any output.
                try:
                    child.stdin.close()
                except BrokenPipeError:
                    pass
                try:
                    child.wait(timeout=38)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                if stop_requested():
                    write_status("Stopping the interpreter.", sys.stdout)
                if approval is not None:
                    report_paid_shutdown(child.returncode)
                elif child.returncode:
                    raise RuntimeError("The local rehearsal service exited unexpectedly.")


def request_stop(_number, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--live", action="store_true", help="Start live interpretation · billed by your own OpenAI account")
    mode.add_argument("--check", action="store_true", help="verify key-free startup and shutdown")
    mode.add_argument("--check-live-config", action="store_true", help="verify the one-hour live configuration with an inert test value")
    mode.add_argument("--check-idle-expiry", action="store_true", help="wait for a one-minute live configuration to expire with no browser or provider work")
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
    # During consent a signal cancels input. Once approved, every signal latches
    # the same Stop request. Nothing can interrupt the final wait or its receipt.
    stopping = False

    def latch_stop(_number, _frame):
        nonlocal stopping
        stopping = True

    def stop_requested():
        return stopping

    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, latch_stop)
    if args.check_idle_expiry:
        run((1, 7, "fictional-key-never-transmitted"), True, wait_for_expiry=True, stop_requested=stop_requested)
    else:
        run(approval, args.check or args.check_live_config, stop_requested=stop_requested)


if __name__ == "__main__":
    try:
        main()
    except (EOFError, KeyboardInterrupt):
        sys.exit("Session canceled.")
    except UnconfirmedShutdown as error:
        write_status(str(error), sys.stderr, sys.stdout)
        sys.exit(1)
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        sys.exit(f"Interpreter: {error}")
