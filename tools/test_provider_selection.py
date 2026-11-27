"""Exercise one-use vendor binding and credential selection using inert strings only."""
import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import call_pilot_activation as activation


class ProviderSelection(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.executable = self.root / 'service'
        self.executable.write_text('fixture')
        self.executable.chmod(0o700)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        (self.assets / 'index.html').write_text('fixture')
        self.attestations = dict.fromkeys(activation.CHECKS, True)
        self.profile = self.root / 'activation.json'
        self.config = self.root / 'config.json'
        activation._write(self.config, {'assets_dir': str(self.assets), 'duration_seconds': 120,
                                        'repeatable_sessions': False, 'provider': None})
        self.openai = 'openai-value-for-unit-test-only'
        self.google = 'gemini-value-for-unit-test-only'

    def keys(self, path, body):
        path.write_text(body)
        path.chmod(0o600)

    def prepare(self, vendor, duration_seconds=120):
        return activation.prepare_profile(self.profile, executable=self.executable, assets=self.assets,
            duration_seconds=duration_seconds, budget='0.45', routes=['transcription', 'live'],
            live_modes=['interpretation'], max_sessions=2, attestations=self.attestations, vendor=vendor)

    def start(self):
        with patch.object(activation, 'ROOT', self.root):
            return activation.activate(self.profile, executable=self.executable,
                                       config=self.config, stop_check=lambda: None)

    def test_vendor_is_bound_before_key_loading_and_once_on_activation(self):
        profile = self.prepare('gemini')
        self.assertEqual(profile['version'], 5)
        self.assertEqual(activation.read_profile(self.profile)['vendor'], 'gemini')
        self.keys(self.root / '.env', f'OPENAI_API_KEY={self.openai}')
        self.keys(self.root / '.env.gemini', f'OPENAI_API_KEY={self.openai}\nGEMINI_API_KEY={self.google}')
        output = self.start()
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        provider = json.loads(output.read_text())['provider']
        self.assertEqual((provider['vendor'], provider['api_key']), ('gemini', self.google))
        self.assertNotIn(self.openai, output.read_text())
        with self.assertRaises(FileExistsError):
            self.start()

    def test_gemini_profile_duration_boundary_and_tampered_profile(self):
        for seconds in (601, 660):
            with self.subTest(seconds=seconds), self.assertRaisesRegex(ValueError, '10 minutes'):
                self.prepare('gemini', seconds)
            self.assertFalse(self.profile.exists())
        self.prepare('gemini', 600)
        limits = activation.read_profile(self.profile)['limits']
        self.assertEqual((limits['total_duration_seconds'], limits['route_duration_seconds']), (600, 600))
        self.profile.unlink()
        openai = self.prepare('openai', 660)
        self.assertEqual(activation.read_profile(self.profile)['limits']['route_duration_seconds'], 660)
        openai.update(version=5, vendor='gemini')
        self.profile.write_text(json.dumps(openai))
        with self.assertRaisesRegex(ValueError, '10 minutes'):
            activation.read_profile(self.profile)

    def test_gemini_refuses_an_openai_only_file_without_fallback(self):
        self.prepare('gemini')
        for name in ('.env', '.env.gemini'):
            self.keys(self.root / name, f'OPENAI_API_KEY={self.openai}')
        with self.assertRaisesRegex(ValueError, 'exactly_one'):
            self.start()
        self.assertFalse((self.root / 'provider-config.json').exists())

    def test_a_legacy_profile_cannot_acquire_a_second_vendor(self):
        original = self.prepare('openai')
        self.assertEqual(original['version'], 3)
        self.assertNotIn('vendor', original)
        self.keys(self.root / '.env', f'OPENAI_API_KEY={self.openai}')
        provider = json.loads(self.start().read_text())['provider']
        self.assertNotIn('vendor', provider)
        self.assertEqual(provider['api_key'], self.openai)
        self.profile.write_text(json.dumps({**original, 'vendor': 'gemini'}))
        with self.assertRaises(ValueError):
            activation.read_profile(self.profile)

    def test_obvious_vendor_mismatch_and_unsupported_scope_are_refused(self):
        path = self.root / '.env.gemini'
        self.keys(path, 'GEMINI_API_KEY=sk-fictional-placeholder')
        with self.assertRaisesRegex(ValueError, 'family_mismatch'):
            activation.read_approved_key(path, 'gemini')
        with self.assertRaises(ValueError):
            activation.validate_gemini_scope(['live'], ['assistant'], 1)


def check_untimed_target_words(page):
    """Drive synthetic native event shapes through the authenticated operator client."""
    from benchmark_interpretation import TARGET_ROWS, read_target_words
    from quality.fixture import screen
    from playwright.sync_api import expect
    from test_browser_session import observe_operator_stream
    page.get_by_text('Advanced audio & manual routes', exact=True).click()
    observe_operator_stream(page)
    page.get_by_role('button', name='Connect private stream', exact=True).click()
    page.wait_for_function("() => typeof window.sendCaptionFixture === 'function'")
    events = []
    def utterance(key, source, route, mode, language, provenance, text):
        lane = {"route_id": key, "source": source, "route": route}
        events.append({**lane, "type": "route", "live_mode": mode, "language": language, "state": "ready",
                       "code": None, "expires_in_seconds": 60})
        events.append({**lane, "type": "transcript", "provenance": provenance, "text": text,
                       "final": route == "transcription", "item": {"id": "utterance_1", "revision": 1, "sequence": 1}
                       if route == "transcription" else None})
    utterance("fixture_src", "self", "transcription", None, "en", "source_asr", "Unrelated source words")
    utterance("fixture_wrong_target", "self", "live", "interpretation", "hi", "live_output", "Other target")
    utterance("fixture_wrong_source", "remote_mix", "live", "interpretation", "ja", "live_output", "Other source")
    first = "エニーは金曜日に十五箱。訂正、木曜日に五十箱で"
    tail = "はない。"
    utterance("fixture_target", "self", "live", "interpretation", "ja", "live_output", first)
    events.append({**events[-1], "text": tail})
    page.evaluate("(events) => window.sendCaptionFixture(events)", events)
    expect(page.locator(TARGET_ROWS)).to_have_text([first, tail])
    target = read_target_words(page)
    assert target == first + tail
    assert not all(screen(target).values()), "Split negation was lost when reading the provider output"


def browser_preview(vendor):
    """Verify source activation and the WASM consent surface before opening a paid route."""
    from playwright.sync_api import expect, sync_playwright
    import local_call_pilot as launcher

    origin = "http://127.0.0.1:5080"
    gemini = vendor == "gemini"
    seconds, limit, payer = (120, "0.45", "Google") if gemini else (3600, "5.00", "OpenAI")
    with launcher.ownership(5080), tempfile.TemporaryDirectory(dir=launcher.local_dir()) as temporary:
        root = Path(temporary)
        server, config = launcher.prepare(root, seconds, port=5080)
        metadata = json.loads(config.read_text())
        profile = root / "activation.json"
        activation.prepare_profile(profile, executable=server, assets=Path(metadata["assets_dir"]),
            duration_seconds=seconds, budget=limit, routes=["transcription", "live"], live_modes=["interpretation"],
            max_sessions=2, attestations=dict.fromkeys(activation.CHECKS, True), vendor=vendor)
        fake = "fixture-only-unusable-value"
        key = root / (".env.gemini" if gemini else ".env")
        key.write_text(("GEMINI_API_KEY=" if gemini else "OPENAI_API_KEY=") + fake)
        key.chmod(0o600)
        with patch.object(activation, "ROOT", root):
            selected = activation.activate(profile, executable=server, config=config, stop_check=lambda: None)
        process = subprocess.Popen([server, "--config", selected], stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=root, env={"PATH": "/usr/bin:/bin", "LANG": "C"})
        try:
            launcher.wait_ready(process, root, providers_enabled=True, origin=origin)
            selected.unlink()
            with sync_playwright() as driver:
                browser = driver.chromium.launch(channel="chrome")
                page = browser.new_page()
                admissions = []
                page.on("request", lambda r: admissions.append(r.url) if r.url == origin + "/api/routes" else None)
                with page.expect_response(lambda r: r.url == origin + "/api/bootstrap") as boot, \
                     page.expect_response(lambda r: r.url == origin + "/api/budget") as fees:
                    page.goto(launcher.bootstrap_url(root, origin=origin))
                from benchmark_interpretation import initial, matches_live_activation, observe_http
                observed = initial(True, vendor)
                observed["local_estimate_limit_microusd"] = 450_000 if gemini else 5_000_000
                observe_http(boot.value, observed)
                observe_http(fees.value, observed)
                assert matches_live_activation(observed), "Benchmark failed to verify the actual local provider selection"
                session, budget = boot.value.json(), fees.value.json()
                assert fake not in json.dumps([session, budget])
                caps = session["capabilities"]
                assert session.get("provider_vendor", "openai") == vendor and caps["source_captions"] and caps["live_interpreter"]
                assert not caps["live_assistant"] and not caps["prepared_public_speech"]
                assert {r["model"] for r in budget["routes"] if r["configured"]} == (
                    {"gemini-3.5-transcribe-live", "gemini-3.5-live-translate-preview"} if gemini else {"gpt-live-transcribe", "gpt-live-1"})
                page.locator("#meet-language").select_option("ja")
                start = page.get_by_role("button", name=f"Start live interpretation · billed by your own {payer} account", exact=True)
                estimate = page.locator("#meet-start-estimate")
                expect(start).to_be_enabled()
                expect(estimate).to_be_visible()
                # The finite clock starts in the launcher. Chrome quotes the time still available.
                title = estimate.locator("strong").inner_text()
                time = re.search(r"(\d+) min(?: (\d+) sec)?", title)
                assert time, title
                remaining = int(time[1]) * 60 + int(time[2] or 0)
                assert 0 < remaining <= seconds
                # Rates for both routes together, in tenths of a cent per minute.
                tenths = 200 if gemini else 67
                cents = (remaining * tenths + 599) // 600
                assert f"US${cents // 100}.{cents % 100:02d}" in title, title
                expect(estimate).to_contain_text("US$" + limit)
                expect(estimate).to_contain_text("Enny charges no fee")
                expect(estimate).to_contain_text("account that owns your API key")
                assert "meet-start-estimate" in start.get_attribute("aria-describedby").split()
                page.set_viewport_size({"width": 390, "height": 844})
                assert start.evaluate("(b) => b.scrollWidth <= b.clientWidth && b.getBoundingClientRect().right <= innerWidth")
                if gemini:
                    check_untimed_target_words(page)
                else:
                    # Replace only the local quote response. A failed refresh must revoke paid Start.
                    page.get_by_text("Cost & limits", exact=True).click()
                    budget_url = origin + "/api/budget"
                    page.route(budget_url, lambda route: route.fulfill(status=503, body=""))
                    page.locator("#refresh-budget").click()
                    expect(start).to_be_disabled()
                    expect(estimate).to_contain_text("unavailable")
                    page.unroute(budget_url)
                    page.locator("#refresh-budget").click()
                    expect(start).to_be_enabled()
                with page.expect_response(lambda r: r.url == origin + "/api/stop-receipt" and r.ok) as closed:
                    with page.expect_response(lambda r: r.url == origin + "/api/stop" and r.ok) as stop:
                        page.locator("header button.stop-button").click()
                observe_http(stop.value, observed)
                observe_http(closed.value, observed)
                assert observed["provider_close_observation"] == "none_admitted"
                assert observed["stop_reserved_microusd"] == 0 and observed["provider_invoiced_usd"] is None
                expect(page.locator('[data-rust-ui="stop-receipt"]')).to_be_visible()
                assert not admissions, "Provider selection unexpectedly opened a route"
                browser.close()
        finally:
            launcher.stop_owned(process, providers_enabled=True)
    print(f"Verified {payer} payer, estimate and local limit before start. Stop confirmed zero admissions and reservations.")


if __name__ == '__main__':
    if sys.argv[1:] == ["--browser"]:
        for vendor in ("openai", "gemini"):
            browser_preview(vendor)
    else:
        unittest.main()
