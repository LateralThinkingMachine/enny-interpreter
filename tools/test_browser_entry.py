"""Checks for the self-hosted interpreter's local credentials-file handoff."""
import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import local_call_pilot as pilot


class BrowserEntryTest(unittest.TestCase):
    def linux_entry(self, env, browser_available=True):
        directory = Path('/private/example-session')
        output = io.StringIO()
        with patch.object(pilot.sys, 'platform', 'linux'), patch.dict(os.environ, env, clear=True), \
                patch.object(os, 'access', return_value=browser_available), \
                patch.object(subprocess, 'Popen') as launch, contextlib.redirect_stdout(output):
            pilot.browser_entry(directory, open_browser=True, origin=pilot.ORIGIN)
        return output.getvalue(), launch

    def test_linux_opens_only_nonsecret_file_without_inheriting_provider_keys(self):
        output, launch = self.linux_entry({'DISPLAY': ':0', 'OPENAI_API_KEY': 'test-only-value'})
        launch.assert_called_once()
        args, options = launch.call_args
        self.assertEqual(args[0], ['/usr/bin/google-chrome', (pilot.ROOT / 'tools/open-operator.html').as_uri()])
        self.assertNotIn('OPENAI_API_KEY', options['env'])
        self.assertNotIn('test-only-value', output)
        self.assertIn('credentials.json', output)

    def test_headless_and_missing_linux_browser_leave_a_manual_file_entry(self):
        for env, available in [({}, True), ({'DISPLAY': ':0'}, False)]:
            with self.subTest(env=env):
                output, launch = self.linux_entry(env, available)
                launch.assert_not_called()
                self.assertIn('open-operator.html', output)
                self.assertIn('credentials.json', output)

    def test_macos_uses_existing_private_chrome_handoff(self):
        with patch.object(pilot.sys, 'platform', 'darwin'), patch.object(pilot, 'open_chrome') as chrome:
            pilot.browser_entry(Path('/private/example-session'), open_browser=True, origin=pilot.ORIGIN)
        chrome.assert_called_once_with(Path('/private/example-session'), origin=pilot.ORIGIN)


class ProviderShutdownTest(unittest.TestCase):
    def test_successful_interrupt_waits_for_confirmed_shutdown(self):
        child = Mock()
        child.poll.return_value = None
        child.wait.return_value = 0
        pilot.stop_owned(child, providers_enabled=True)
        child.terminate.assert_called_once()
        child.kill.assert_not_called()

    def test_abnormal_or_forced_paid_shutdown_is_reported_as_uncertain(self):
        for still_running in (False, True):
            with self.subTest(still_running=still_running):
                child = Mock()
                child.poll.return_value = None if still_running else 1
                child.wait.side_effect = [subprocess.TimeoutExpired('local service', 35), -9]
                error = io.StringIO()
                with contextlib.redirect_stderr(error), self.assertRaises(RuntimeError):
                    pilot.stop_owned(child, providers_enabled=True)
                self.assertIn('Check your provider account', error.getvalue())
                self.assertEqual(child.kill.called, still_running)


def check_in_chrome():
    from playwright.sync_api import Error as BrowserError, expect, sync_playwright

    origin, _ = pilot.port_pair(5080)
    with pilot.ownership(5080), tempfile.TemporaryDirectory(dir=pilot.local_dir()) as temp:
        directory = Path(temp)
        binary, config = pilot.prepare(directory, 120, port=5080)
        process = subprocess.Popen([str(binary), '--config', str(config)], cwd=directory,
                                   env={'PATH': '/usr/bin:/bin', 'LANG': 'C'}, stdin=subprocess.DEVNULL)
        try:
            pilot.wait_ready(process, directory, providers_enabled=False, origin=origin)
            config.unlink()
            with sync_playwright() as p:
                browser = p.chromium.launch(channel='chrome', headless=True)
                try:
                    context = browser.new_context()
                    page = context.new_page()
                    helper = (pilot.ROOT / 'tools/open-operator.html').as_uri()
                    # The helper only accepts validated local service coordinates.
                    page.goto(helper)
                    for invalid in [
                        {'origin': 'https://example.invalid', 'bootstrap': '1' * 64},
                        {'origin': origin, 'bootstrap': 'invalid'},
                    ]:
                        page.locator('#entry').set_input_files({
                            'name': 'credentials.json', 'mimeType': 'application/json',
                            'buffer': json.dumps(invalid).encode(),
                        })
                        expect(page.get_by_role('status')).to_contain_text('Select the credentials.json')
                        expect(page).to_have_url(helper)
                    page.locator('#entry').set_input_files(directory / 'credentials.json')
                    expect(page.locator('#private-notes')).to_be_visible()
                    expect(page).to_have_url(origin + '/')
                    isolated = context.new_page()
                    isolated.goto(helper)
                    isolated.locator('#entry').set_input_files(directory / 'credentials.json')
                    expect(isolated.get_by_text('No valid local session.', exact=False)).to_be_visible()
                    expect(isolated).to_have_url(origin + '/')
                    assert not context.cookies()
                except (AssertionError, BrowserError):
                    # A browser assertion can contain the one-use URL before frontend cleanup.
                    raise RuntimeError('The private browser entry check did not complete.') from None
                finally:
                    browser.close()
        finally:
            pilot.stop_owned(process)
    print('Passed file selection, loopback operator login, URL cleanup and one-use replay denial.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--chrome', action='store_true', help='Also exercise a built, provider-disabled service in a clean desktop Chrome profile.')
    args = parser.parse_args()
    result = unittest.main(argv=['test_browser_entry'], exit=False).result
    if not result.wasSuccessful():
        raise SystemExit(1)
    if args.chrome:
        check_in_chrome()
