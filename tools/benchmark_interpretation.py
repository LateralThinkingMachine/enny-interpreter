"""Evaluate a fixed synthetic two-speaker correction through the native interpreter."""
import argparse
import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

from call_pilot_activation import build_identity, limits_for, read_approved_key
from local_call_pilot import bootstrap_url, local_dir, ownership, prepare, wait_ready
from quality.fixture import ID, TURNS, screen, synthesize

HERE = Path(__file__).resolve().parent
ORIGIN = "http://127.0.0.1:5080"
DURATION = 120
BENCHMARK_PROVIDERS = {
    "openai": ("gpt-live-1", 10_000_000),
    "gemini": ("gemini-3.5-live-translate-preview", 450_000),
}
STOP_OBSERVATION_SECONDS = 12
TARGET_ROWS = 'li[data-interpretation-target="ja"][data-interpretation-source="self"][data-interpretation-mode="fixed"] > p'


def initial(live, vendor="openai"):
    model, budget = BENCHMARK_PROVIDERS[vendor]
    return {"fixture": ID, "mode": "live" if live else "key-free rehearsal",
            "source_language": "en", "target_language": "ja", "provider": vendor if live else None, "model": model if live else None,
            "observed_provider": None, "observed_model": None, "budget_generation": None,
            "observed_budget_limit_microusd": None, "input_text": [words for _, words in TURNS], "stage": "preflight",
            "start_attempts": 0, "target_text": "", "first_playback_ms": None,
            "capture_source": "synthetic microphone", "input_ended": False, "operator_input_meter_observed": False, "output_energy_observed": False,
            "local_estimate_limit_microusd": budget if live else 0,
            "largest_observed_reservation_microusd": None,
            "provider_invoiced_usd": None, "server_stop_acknowledged": False,
            "native_shutdown": "not_started", "provider_close_observation": "not_observed",
            "session_generation": None, "stop_generation": None, "stop_reserved_microusd": None,
            "synthetic_capture_tracks_ended": None, "stop_observation_window_seconds": STOP_OBSERVATION_SECONDS,
            "playback_recording": "not_requested", "target_text_status": "not_observed", "meaning_review": "not_reviewed"}


def matches_live_activation(receipt):
    return (receipt["session_generation"] is not None
            and receipt["session_generation"] == receipt["budget_generation"]
            and receipt["observed_provider"] == receipt["provider"]
            and receipt["observed_model"] == receipt["model"]
            and receipt["observed_budget_limit_microusd"] == receipt["local_estimate_limit_microusd"])


def classify(receipt):
    if receipt.get("failure") or not receipt["server_stop_acknowledged"] or receipt["native_shutdown"] != "clean":
        return "incomplete"
    if receipt["playback_recording"] not in ("not_requested", "saved"):
        return "incomplete"
    if not receipt["operator_input_meter_observed"] or receipt["synthetic_capture_tracks_ended"] is not True:
        return "incomplete"
    expected_provider = "none_admitted" if receipt["mode"] == "key-free rehearsal" else "close_observed"
    if receipt["provider_close_observation"] != expected_provider:
        return "incomplete"
    if receipt["mode"] == "key-free rehearsal":
        return "rehearsal_passed" if receipt["input_ended"] and not receipt["output_energy_observed"] else "incomplete"
    if (not matches_live_activation(receipt) or not receipt["input_ended"] or not receipt["output_energy_observed"] or not receipt["target_text"]
            or receipt["target_text_status"] != "last_visible_provider_deltas_after_bounded_drain"
            or type(receipt["first_playback_ms"]) is not int or receipt["first_playback_ms"] < 0):
        return "incomplete"
    if not all(receipt["lexical_screen"].values()):
        return "inconclusive_lexical_screen"
    return "lexical_match_requires_bilingual_review"


@contextmanager
def new_receipt(path):
    """Reserve the run before reading a key or opening a route. Never overwrite an earlier run."""
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
        file.write('{"stage":"reserved","result":"interrupted_or_in_progress"}\n')
        file.flush()
        os.fsync(file.fileno())
        yield file


def observe_http(response, receipt):
    path = response.url.removeprefix(ORIGIN)
    if path not in ("/api/bootstrap", "/api/routes", "/api/budget", "/api/diagnostics",
                    "/api/stop", "/api/stop-receipt"):
        return
    if not response.ok:
        receipt.setdefault("local_http_failures", []).append([path, response.status])
        return
    if path == "/api/routes":
        return
    try:
        body = response.json()
    except Exception:
        return
    if not isinstance(body, dict):
        return
    if path == "/api/bootstrap":
        # Only these numeric fields leave the authenticated response.
        generation = body.get("session_generation", body.get("generation"))
        if body.get("role") == "operator" and type(generation) is int and generation > 0:
            receipt["session_generation"] = generation
            if body.get("providers_enabled") is True and body.get("continuous", False) is False:
                vendor = body.get("provider_vendor", "openai")
                if vendor in BENCHMARK_PROVIDERS:
                    receipt["observed_provider"] = vendor
        return
    session = receipt["session_generation"]
    if path == "/api/stop":
        terminal = body.get("generation")
        if session is not None and type(terminal) is int and terminal == session + 1 and body.get("state") == "stopped":
            receipt["server_stop_acknowledged"] = True
            receipt["stop_generation"] = terminal
        return
    if path == "/api/stop-receipt":
        reserved = body.get("reserved_microusd")
        if (not receipt["server_stop_acknowledged"] or type(body.get("version")) is not int or body["version"] != 1
                or type(body.get("session_generation")) is not int or body["session_generation"] != session
                or type(body.get("generation")) is not int or body["generation"] != receipt["stop_generation"]
                or body.get("provider_activity") not in ("none_admitted", "close_observed", "unconfirmed")
                or body.get("invoice") != "unavailable" or "reserved_microusd" not in body
                or not (reserved is None or type(reserved) is int and 0 <= reserved <= receipt["local_estimate_limit_microusd"])
                or body["provider_activity"] == "none_admitted" and reserved != 0
                or body["provider_activity"] == "close_observed" and (reserved is None or reserved == 0)):
            return
        receipt["provider_close_observation"] = body["provider_activity"]
        receipt["stop_reserved_microusd"] = reserved
        return
    if path == "/api/budget":
        generation = body.get("generation")
        if type(generation) is int and generation > 0 and body.get("state") == "active":
            rows = [r for r in body.get("routes", []) if isinstance(r, dict) and r.get("route") == "live"
                    and r.get("configured") is True]
            model = rows[0].get("model") if len(rows) == 1 else None
            if model in [value[0] for value in BENCHMARK_PROVIDERS.values()]:
                receipt["observed_model"] = model
                receipt["budget_generation"] = generation
                limit = (body.get("amounts") or {}).get("approved_microusd")
                receipt["observed_budget_limit_microusd"] = limit if type(limit) is int else None
        seen = (body.get("amounts") or {}).get("reserved_microusd")
        if type(seen) is int:
            receipt["largest_observed_reservation_microusd"] = max(receipt["largest_observed_reservation_microusd"] or 0, seen)
    else:
        issue = (body.get("last_issue") or {}).get("code")
        if isinstance(issue, str) and re.fullmatch(r"[a-z_]{1,64}", issue):
            receipt["native_issue"] = issue


def read_target_words(page):
    # Provider deltas can split a word or a negation at any character.
    return "".join(page.locator(TARGET_ROWS).all_text_contents())


def retain_target(receipt, text):
    receipt["target_text"] = text[:8000]
    if len(text) > 8000:
        receipt["target_text_status"] = "truncated"
    elif text:
        receipt["target_text_status"] = "last_visible_provider_deltas_after_bounded_drain"


def save_recording(receipt, output, encoded):
    if output is None:
        return
    data = base64.b64decode(encoded or "", validate=True)
    if not data.startswith(bytes.fromhex("1a45dfa3")) or len(data) <= 4:
        receipt["playback_recording"] = "unavailable"
        return
    output.write(data)
    output.flush()
    receipt["playback_recording"] = "saved"


def observe_terminal_receipt(page, receipt, clock=time.monotonic):
    """Use the operator's read-only receipt control after automatic checks have drained."""
    started = clock()
    deadline = started + STOP_OBSERVATION_SECONDS
    next_read = started + 1.5
    refresh = page.locator('[data-rust-ui="stop-receipt"]').get_by_role("button", name="Check Stop receipt")
    while receipt["provider_close_observation"] in ("not_observed", "unconfirmed"):
        now = clock()
        if now >= deadline:
            return
        if now >= next_read and refresh.is_visible() and refresh.is_enabled():
            try:
                refresh.click(timeout=max(1, int(min(0.3, deadline - now) * 1000)))
            except Exception:
                # An automatic read can disable the same button between checks.
                pass
            next_read = clock() + 1
        page.wait_for_timeout(100)


def run_browser(browser, directory, live, pcm, receipt, audio_output):
    page = browser.new_page()
    page.set_default_timeout(8000)
    page.add_init_script(path=HERE / "quality/audio_probe.js")
    page.on("response", lambda response: observe_http(response, receipt))
    page.on("dialog", lambda dialog: dialog.dismiss())
    try:
        receipt["stage"] = "bootstrap"
        url = bootstrap_url(directory, origin=ORIGIN)
        if live:
            with page.expect_response(lambda response: response.url == ORIGIN + "/api/bootstrap") as session_response, \
                 page.expect_response(lambda response: response.url == ORIGIN + "/api/budget") as budget_response:
                page.goto(url, wait_until="domcontentloaded")
            observe_http(session_response.value, receipt)
            observe_http(budget_response.value, receipt)
            if not matches_live_activation(receipt):
                raise RuntimeError("provider_activation_mismatch")
        else:
            page.goto(url, wait_until="domcontentloaded")
        page.locator("#meet-source").select_option("self")
        page.locator("#meet-language").select_option("ja")
        if live:
            receipt["stage"] = "admit_native_routes"
            receipt["start_attempts"] += 1
            page.locator("#start-meet").click()
            page.wait_for_function("""() => /^(Listening|Setup needs attention|AI paused)/.test(
                document.querySelector('.meet-progress strong')?.textContent || '')""", timeout=23000)
            if not page.locator(".meet-progress strong").inner_text().startswith("Listening"):
                raise RuntimeError("native_routes_unavailable")
        else:
            control = page.locator("#capture-agreement")
            control.evaluate("e => { const d = e.closest('details'); if (d) d.open = true; }")
            control.check()
            page.locator("#choose-meet-tab").click()
            page.wait_for_function("() => document.querySelector('.meet-progress strong')?.textContent.includes('Microphone selected')")
        receipt["stage"] = "send_synthetic_speech"
        page.evaluate("pcm => window.qualityFixture.play(pcm)", base64.b64encode(pcm).decode("ascii"))
        page.wait_for_function("() => document.querySelector('#meet-audio-level')?.value > 0", timeout=6000)
        receipt["operator_input_meter_observed"] = True
        page.wait_for_function("() => window.qualityFixture.timing().input_ended", timeout=26000)
        receipt["stage"] = "observe_interpretation"
        # Preserve a bounded window for late corrections instead of ending on the first word.
        end = time.monotonic() + (14 if live else 0)
        while time.monotonic() < end:
            page.wait_for_timeout(250)
        timing = page.evaluate("() => window.qualityFixture.timing()")
        receipt["input_ended"] = timing["input_ended"]
        receipt["first_playback_ms"] = timing["first_playback_ms"]
        receipt["output_energy_observed"] = timing["first_playback_ms"] is not None
        retain_target(receipt, read_target_words(page))
    finally:
        # Request Stop while capture and the app's normal audio graph are still attached.
        try:
            with page.expect_response(lambda resp: resp.url == ORIGIN + "/api/stop", timeout=6000):
                page.locator("header button.stop-button").click(timeout=2500)
            observe_terminal_receipt(page, receipt)
            # Observe before finish() releases the fixture's own copy of the track.
            receipt["synthetic_capture_tracks_ended"] = page.evaluate("() => window.qualityFixture.tracksEnded()")
        finally:
            try:
                audio = page.evaluate("""() => Promise.race([window.qualityFixture.finish(),
                    new Promise(resolve => setTimeout(() => resolve(null), 5000))])""")
                save_recording(receipt, audio_output, audio)
            finally:
                page.close()


def execute(args, receipt, audio_output):
    from playwright.sync_api import sync_playwright
    with ownership(5080), tempfile.TemporaryDirectory(dir=local_dir()) as temp:
        directory = Path(temp)
        binary, config_path = prepare(directory, DURATION, port=5080)
        config = json.loads(config_path.read_text())
        config_path.unlink()
        receipt["tested_build"] = build_identity(binary, Path(config["assets_dir"]))
        pcm, receipt["synthetic_pcm_sha256"] = synthesize(directory)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(channel="chrome", headless=True,
                args=["--autoplay-policy=no-user-gesture-required", "--mute-audio"])
            receipt["browser"] = browser.version
            try:
                if args.live:
                    config["provider"] = {**limits_for(DURATION, receipt["local_estimate_limit_microusd"],
                        ["transcription", "live"], live_modes=["interpretation"], max_sessions=2),
                        "api_key": read_approved_key(args.key_file.absolute(), args.provider)}
                    if args.provider == "gemini":
                        config["provider"]["vendor"] = args.provider
                wire = json.dumps(config).encode()
                with subprocess.Popen([binary, "--desktop-pipe"], stdin=subprocess.PIPE,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=directory,
                        env={"PATH": "/usr/bin:/bin", "LANG": "C"}) as service:
                    receipt["native_shutdown"] = "pending"
                    try:
                        service.stdin.write(len(wire).to_bytes(4, "big") + wire)
                        service.stdin.flush()
                        del wire
                        config.pop("provider", None)
                        wait_ready(service, directory, providers_enabled=args.live, origin=ORIGIN)
                        run_browser(browser, directory, args.live, pcm, receipt, audio_output)
                    finally:
                        try:
                            service.stdin.close()
                        except BrokenPipeError:
                            pass
                        try:
                            status = service.wait(timeout=38)
                            receipt["native_shutdown"] = "clean" if status == 0 else "unconfirmed"
                        except subprocess.TimeoutExpired:
                            service.kill()
                            service.wait()
                            receipt["native_shutdown"] = "timed_out"
            finally:
                browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="authorize one metered two-minute run with the selected provider")
    parser.add_argument("--provider", choices=tuple(BENCHMARK_PROVIDERS),
                        help="local estimate limit per live run: " + ", ".join(
                            f"{vendor} USD {budget / 1_000_000:.2f}"
                            for vendor, (_, budget) in BENCHMARK_PROVIDERS.items()))
    parser.add_argument("--key-file", type=Path, help="private owner-only .env, used only with --live")
    parser.add_argument("--receipt", required=True, type=Path, help="new private JSON result file")
    parser.add_argument("--save-playback", type=Path, help="new private .webm for bilingual listening review")
    args = parser.parse_args()
    if args.live != bool(args.key_file) or (args.save_playback and not args.live):
        parser.error("--live and --key-file belong together; --save-playback needs a live run")
    if args.save_playback == args.receipt:
        parser.error("Use distinct receipt and playback files")
    if args.provider and not args.live:
        parser.error("--provider selects the destination of an explicitly live run")
    args.provider = args.provider or "openai"
    receipt = initial(args.live, args.provider)
    if args.save_playback:
        receipt["playback_recording"] = "requested"
    with new_receipt(args.receipt) as output:
        audio = None
        try:
            if args.save_playback is not None:
                audio = os.fdopen(os.open(args.save_playback, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb")
            execute(args, receipt, audio)
        except Exception as exc:
            # Browser exceptions may contain the bootstrap fragment or DOM text. Retain only the stage and type.
            receipt["failure"] = type(exc).__name__
        finally:
            if audio:
                audio.close()
            receipt["lexical_screen"] = screen(receipt["target_text"])
            receipt["result"] = classify(receipt)
            output.seek(0)
            output.truncate()
            json.dump(receipt, output, indent=2, ensure_ascii=False)
            output.write("\n")
    print(f"{receipt['result']} ({receipt['stage']})")
    return 0 if receipt["result"] in {"rehearsal_passed", "lexical_match_requires_bilingual_review"} else 1


if __name__ == "__main__":
    sys.exit(main())
