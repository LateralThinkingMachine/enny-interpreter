"""Key-free regression tests for the synthetic quality benchmark."""
import base64
from io import BytesIO
from pathlib import Path
import tempfile
import sys
import unittest

from benchmark_interpretation import classify, initial, matches_live_activation, new_receipt, observe_http, observe_terminal_receipt, read_target_words, retain_target, save_recording
from quality.fixture import screen


GOOD = "エニーさん、出荷は金曜日に十五箱です。訂正します。木曜日に五十箱出荷します。"


class QualityEvidence(unittest.TestCase):
    def receipt(self):
        result = initial(True)
        result.update(observed_provider="openai", observed_model="gpt-live-1", session_generation=1,
            budget_generation=1, observed_budget_limit_microusd=10_000_000, input_ended=True, operator_input_meter_observed=True, first_playback_ms=420,
            output_energy_observed=True, native_shutdown="clean", server_stop_acknowledged=True,
            target_text=GOOD, target_text_status="last_visible_provider_deltas_after_bounded_drain",
            lexical_screen=screen(GOOD), provider_close_observation="close_observed", synthetic_capture_tracks_ended=True)
        return result

    def test_corrected_numbers_dates_name_and_script_normalization(self):
        self.assertTrue(all(screen(GOOD).values()))
        self.assertTrue(all(screen("Enny、金曜に１５箱。訂正、木曜日に５０箱。").values()))
        self.assertFalse(screen(GOOD.replace("エニー", "サラ"))["name_seen"])
        self.assertFalse(screen(GOOD.replace("十五", "百十五"))["fifteen_then_fifty_boxes"])
        self.assertFalse(screen(GOOD.replace("五十", "五百"))["fifteen_then_fifty_boxes"])

    def test_default_fixture_identifies_enny(self):
        receipt = initial(False)
        self.assertEqual(receipt["fixture"], "en-ja-shipment-correction-v2")
        self.assertTrue(receipt["input_text"][0].startswith("Enny,"))
        for spelling in ("Enny", "Ｅｎｎｙ", "エニー", "エニ", "えにー", "ｴﾆｰ"):
            with self.subTest(spelling=spelling):
                self.assertTrue(screen(spelling + "さん、")["name_seen"])
        for other in ("Mira", "ミラ", "Penny", "Ennyson", "エニシダ", "えにし"):
            with self.subTest(other=other):
                self.assertFalse(screen(other + "、")["name_seen"])

    def test_old_values_missing_reversed_or_negated_do_not_qualify(self):
        for text in ("エニー、木曜に五十箱", GOOD + "訂正。金曜日に十五箱です。",
                     GOOD.replace("木曜日に五十箱出荷します", "木曜日ではなく五十箱でもありません"),
                     GOOD.replace("五十箱出荷します", "五十箱ではありません")):
            with self.subTest(text=text):
                self.assertFalse(all(screen(text).values()))

    def test_lexical_match_never_substitutes_for_audio_or_semantic_review(self):
        self.assertEqual(classify(self.receipt()), "lexical_match_requires_bilingual_review")
        for missing in ({"observed_provider": "gemini"}, {"budget_generation": 2}, {"observed_model": None},
                        {"observed_budget_limit_microusd": 150_000}, {"observed_budget_limit_microusd": 450_000},
                        {"output_energy_observed": False}, {"first_playback_ms": None},
                        {"first_playback_ms": -1}, {"operator_input_meter_observed": False},
                        {"server_stop_acknowledged": False}, {"native_shutdown": "timed_out"},
                        {"target_text_status": "not_observed"}, {"input_ended": False},
                        {"provider_close_observation": "unconfirmed"}, {"synthetic_capture_tracks_ended": False}):
            self.assertEqual(classify({**self.receipt(), **missing}), "incomplete")
        receipt = self.receipt()
        receipt["lexical_screen"] = screen("訂正、週の四日目に出荷する。")
        self.assertEqual(classify(receipt), "inconclusive_lexical_screen")

    def test_bound_stop_evidence_requires_the_matching_session_and_separate_receipt(self):
        from types import SimpleNamespace
        result = initial(True)
        def observed(path, data):
            observe_http(SimpleNamespace(url="http://127.0.0.1:5080/api/" + path, ok=True,
                                         json=lambda: data), result)
        observed("bootstrap", {"role": "operator", "generation": 3, "csrf": "private-fixture"})
        observed("stop", {"state": "stopped", "generation": 7})
        self.assertFalse(result["server_stop_acknowledged"])
        observed("stop", {"state": "stopped", "generation": 4})
        self.assertTrue(result["server_stop_acknowledged"])
        expected = {"version": 1, "generation": 4, "session_generation": 3, "provider_activity": "close_observed",
                    "reserved_microusd": 15000, "invoice": "unavailable"}
        for change in ({"session_generation": True}, {"generation": 6}, {"provider_activity": "closed"},
                       {"invoice": "paid"}, {"reserved_microusd": -1}, {"reserved_microusd": 10_000_001},
                       {"reserved_microusd": 0}, {"reserved_microusd": None}, {"provider_activity": "none_admitted"}):
            observed("stop-receipt", {**expected, **change})
            self.assertEqual(result["provider_close_observation"], "not_observed")
        for reserved in (None, 0, 15000):
            observed("stop-receipt", {**expected, "provider_activity": "unconfirmed", "reserved_microusd": reserved})
            self.assertEqual(result["provider_close_observation"], "unconfirmed")
        observed("stop-receipt", expected)
        self.assertEqual((result["provider_close_observation"], result["stop_reserved_microusd"]), ("close_observed", 15000))
        observed("stop-receipt", {**expected, "reserved_microusd": 10_000_000})
        self.assertEqual(result["stop_reserved_microusd"], 10_000_000)
        self.assertIsNone(result["provider_invoiced_usd"])
        self.assertNotIn("private-fixture", str(result))

    def test_preflight_requires_the_native_vendor_and_exact_live_route_quote(self):
        from types import SimpleNamespace
        r = initial(True, "gemini")
        def observed(path, body):
            observe_http(SimpleNamespace(url="http://127.0.0.1:5080/api/" + path, ok=True, json=lambda: body), r)
        observed("bootstrap", {"role": "operator", "providers_enabled": True, "provider_vendor": "gemini", "generation": 3})
        quote = {"state": "active", "generation": 3, "amounts": {"approved_microusd": 450000},
                 "routes": [{"route": "live", "model": "gemini-3.5-live-translate-preview", "configured": True}]}
        self.assertFalse(matches_live_activation(r))
        observed("budget", {**quote, "generation": 2})
        self.assertFalse(matches_live_activation(r))
        observed("budget", quote)
        self.assertTrue(matches_live_activation(r))
        for changed in ({"observed_provider": "openai"}, {"observed_budget_limit_microusd": 500000},
                        {"observed_model": "gpt-live-1"}, {"session_generation": None}):
            self.assertFalse(matches_live_activation({**r, **changed}))

    def test_split_target_rows_preserve_the_model_words_and_negation(self):
        from unittest.mock import Mock
        page = Mock()
        for phrase, expected in ((GOOD, True), ("エニーは金曜日に十五箱。訂正、木曜日に五十箱ではない。", False)):
            for split in range(1, len(phrase)):
                page.locator.return_value.all_text_contents.return_value = [phrase[:split], phrase[split:]]
                words = read_target_words(page)
                self.assertEqual(words, phrase)
                self.assertEqual(all(screen(words).values()), expected)

    def test_benchmark_resamples_slow_peer_closure_without_reopening_capture(self):
        from unittest.mock import Mock
        for peer_close in (3, None):
            now = [0.0]
            receipt = {"provider_close_observation": "unconfirmed"}
            page = Mock()
            button = page.locator.return_value.get_by_role.return_value
            page.wait_for_timeout.side_effect = lambda ms: now.__setitem__(0, now[0] + ms / 1000)
            def refresh(**kwargs):
                if peer_close is not None and now[0] >= peer_close:
                    receipt["provider_close_observation"] = "close_observed"
            button.click.side_effect = refresh
            observe_terminal_receipt(page, receipt, lambda: now[0])
            page.locator.return_value.get_by_role.assert_called_once_with("button", name="Check Stop receipt")
            self.assertGreaterEqual(button.click.call_count, 2)
            self.assertLessEqual(now[0], 12.2)
            self.assertEqual(receipt["provider_close_observation"], "close_observed" if peer_close else "unconfirmed")

    def test_truncation_or_missing_requested_recording_blocks_success(self):
        result = self.receipt()
        retain_target(result, GOOD + " " * 8000 + "訂正、金曜日に十五箱")
        self.assertEqual(len(result["target_text"]), 8000)
        self.assertEqual(classify(result), "incomplete")
        result = self.receipt()
        buffer = BytesIO()
        save_recording(result, buffer, None)
        self.assertEqual(result["playback_recording"], "unavailable")
        self.assertEqual(classify(result), "incomplete")
        sample = bytes.fromhex("1a45dfa39f")
        save_recording(result, buffer, base64.b64encode(sample))
        self.assertEqual(buffer.getvalue(), sample)
        self.assertEqual(result["playback_recording"], "saved")

    def test_receipt_is_reserved_once_and_private_before_live_operation(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "run.json"
            with new_receipt(path):
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                original = path.read_bytes()
                with self.assertRaises(FileExistsError), new_receipt(path):
                    self.fail("duplicate reservation was accepted")
                self.assertEqual(path.read_bytes(), original)


def chrome_probe():
    from playwright.sync_api import sync_playwright
    with tempfile.TemporaryDirectory() as temporary, sync_playwright() as ports:
        local = Path(temporary) / "fixture.html"
        local.write_text("<!doctype html><title>Audio measurement control</title>")
        browser = ports.chromium.launch(channel="chrome", headless=True,
            args=["--autoplay-policy=no-user-gesture-required", "--mute-audio"])
        try:
            page = browser.new_page()
            page.add_init_script(path=Path(__file__).parent / "quality/audio_probe.js")
            page.goto(local.as_uri())
            page.evaluate("""async () => {
                await navigator.mediaDevices.getUserMedia({audio: true});
                await window.qualityFixture.play(btoa('\\0'.repeat(3200)));
                const context = window.controlContext = new AudioContext();
                await context.resume();
                window.playControl = amplitude => {
                    const node = context.createBufferSource();
                    node.buffer = context.createBuffer(1, context.sampleRate / 4, context.sampleRate);
                    const samples = node.buffer.getChannelData(0);
                    for (let i = 0; i < samples.length; i++)
                        samples[i] = amplitude * Math.sin(2 * Math.PI * 440 * i / context.sampleRate);
                    node.connect(context.destination); node.start();
                };
                window.playControl(0);
            }""")
            page.wait_for_timeout(300)
            assert page.evaluate("window.qualityFixture.timing().first_playback_ms") is None
            page.evaluate("window.playControl(0.2)")
            page.wait_for_function("() => window.qualityFixture.timing().first_playback_ms !== null")
            result = page.evaluate("window.qualityFixture.timing()")
            assert result["first_playback_ms"] >= 240 and result["input_ended"]
            assert page.evaluate("async () => (await window.qualityFixture.finish())?.length > 100")
            print("Chrome detected rendered tone, rejected silence and produced a playback recording.")
        finally:
            browser.close()


if __name__ == "__main__":
    if sys.argv[1:] == ["--chrome"]:
        chrome_probe()
    else:
        unittest.main()
