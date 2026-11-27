"""Verify the packaged launcher's interactive consent and key input behavior."""
import getpass
import io
import json
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "linux"))
from start import approve_session, main, run, session_limits


class LiveApproval(unittest.TestCase):
    def test_requires_a_terminal(self):
        with patch("sys.stdin.isatty", return_value=False), patch("getpass.getpass") as prompt:
            with self.assertRaisesRegex(ValueError, "interactive terminal"):
                approve_session()
            prompt.assert_not_called()

    def test_declining_stops_before_key_entry(self):
        with patch("sys.stdin.isatty", return_value=True), patch("builtins.input", side_effect=["", "", "yes", "no"]), \
                patch("getpass.getpass") as prompt, redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, "not approved"):
                approve_session()
            prompt.assert_not_called()

    def test_rehearsal_default_announces_zero_cost_without_credentials(self):
        with patch.object(sys, "argv", ["start.py"]), patch("start.signal.signal"), patch("start.run") as launch, \
                patch("getpass.getpass") as key, redirect_stdout(io.StringIO()) as output:
            main()
            self.assertIn("Start free rehearsal", output.getvalue())
            self.assertIn("US$0.00", output.getvalue())
            key.assert_not_called()
            self.assertEqual(launch.call_args.args, (None, False))
            self.assertFalse(launch.call_args.kwargs["stop_requested"]())

    def test_final_cost_confirmation_defaults_to_no_before_key_or_startup(self):
        for refusal in ("", "n", "later"):
            with self.subTest(refusal=refusal), patch.object(sys, "argv", ["start.py", "--live"]), \
                    patch("start.signal.signal"), patch("sys.stdin.isatty", return_value=True), \
                    patch("builtins.input", side_effect=["17", "1.20", "y", "y", "y", "y", refusal]) as inputs, \
                    patch("getpass.getpass") as key, patch("start.run") as launch, redirect_stdout(io.StringIO()) as output:
                with self.assertRaisesRegex(ValueError, "not approved"):
                    main()
                self.assertEqual(inputs.call_args.args[0], "Start live interpretation · billed by your own OpenAI account [y/N] ")
                self.assertIn("Estimated OpenAI cost for 17 minutes on this computer: US$1.14.", output.getvalue())
                self.assertIn("Your local estimate limit: US$1.20.", output.getvalue())
                key.assert_not_called()
                launch.assert_not_called()

    def test_confirmation_uses_hidden_input(self):
        dummy = "a-fictional-unusable-key"
        with patch("sys.stdin.isatty", return_value=True), patch("builtins.input", side_effect=["", "", "y", "y", "y", "y", "yes"]) as checks, \
                patch("getpass.getpass", return_value=dummy), redirect_stdout(io.StringIO()) as output:
            self.assertEqual(approve_session(), (60, 500, dummy))
        self.assertEqual(checks.call_count, 7)
        self.assertIn("Estimated OpenAI cost for 60 minutes on this computer: US$4.02.", output.getvalue())
        self.assertIn("Your local estimate limit: US$5.00.", output.getvalue())
        self.assertNotIn(dummy, output.getvalue())

    def test_settings_repairs_bad_entries_before_approval(self):
        answers = ["0", "61", "1.5", "z", "60", "-1", "5e0", "1.234", "1000.01", "4.01", "4.02"]
        with patch("builtins.input", side_effect=answers), redirect_stdout(io.StringIO()) as output:
            self.assertEqual(session_limits(), (60, 402))
        self.assertIn("US$4.02", output.getvalue())

    def test_custom_minutes_and_cent_boundaries(self):
        for minutes, amount, cents in [(1, "0.07", 7), (2, "0.14", 14), (33, "2.22", 222), (60, "1000", 100000)]:
            with self.subTest(minutes=minutes, amount=amount), patch("builtins.input", side_effect=[str(minutes), amount]), redirect_stdout(io.StringIO()):
                self.assertEqual(session_limits(), (minutes, cents))

    def test_launcher_keeps_service_alive_for_the_approved_meeting(self):
        dummy = "a-fictional-unusable-key"
        for approval, timeout in [((60, 402, dummy), 3760), (None, 640)]:
            with self.subTest(live=bool(approval)), patch("start.subprocess.Popen") as popen, \
                    patch("start.wait_ready") as ready, patch("start.wait_for_end") as wait, patch("start.browser_entry"), redirect_stdout(io.StringIO()):
                child = popen.return_value.__enter__.return_value
                child.returncode = 20 if approval else 0
                run(approval, check=False)
                self.assertEqual(wait.call_args.args[:2], (child, timeout))
                ready.assert_called_once()
                self.assertEqual(ready.call_args.kwargs["providers_enabled"], bool(approval))
                wire, = child.stdin.write.call_args.args
                self.assertEqual(int.from_bytes(wire[:4], "big"), len(wire[4:]))
                frame = json.loads(wire[4:])
                self.assertNotIn(dummy, repr(popen.call_args))
                if approval:
                    self.assertEqual(frame["meeting_minutes"], 60)
                    self.assertEqual(frame["budget_cents"], 402)
                    self.assertEqual(frame["api_key"], dummy)
                    self.assertNotIn("provider", frame)
                else:
                    self.assertEqual(frame["duration_seconds"], 600)
                    self.assertIsNone(frame["provider"])
                    self.assertNotIn("api_key", frame)

    def test_stop_before_spawn_and_failed_spawn_report_no_admission(self):
        approval = (1, 7, "fictional-inert-value")
        for stopped in (True, False):
            with self.subTest(stopped=stopped), patch("start.subprocess.Popen", side_effect=OSError("missing helper")) as spawn, redirect_stdout(io.StringIO()) as output:
                if stopped:
                    run(approval, True, stop_requested=lambda: True)
                    spawn.assert_not_called()
                else:
                    with self.assertRaisesRegex(OSError, "missing helper"):
                        run(approval, True)
                self.assertEqual(output.getvalue().strip(), "Local audio stopped. Enny did not start any OpenAI work.")

    def test_visible_key_entry_is_denied(self):
        with patch("sys.stdin.isatty", return_value=True), patch("builtins.input", side_effect=["60", "4.02", "y", "y", "y", "y", "yes"]), \
                patch("getpass.getpass", side_effect=getpass.GetPassWarning), redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, "cannot conceal"):
                approve_session()


if __name__ == "__main__":
    unittest.main()
