"""Key-free integration check for operator and viewer authentication in Chrome."""
import argparse
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlsplit

from playwright.sync_api import Error as BrowserError, expect, sync_playwright

from local_call_pilot import (
    ORIGIN, PUBLIC_ORIGIN, bootstrap_url, local_dir, ownership, prepare,
    stop_owned, wait_ready, port_pair,
)


def install_rehearsal_inputs(page):
    page.evaluate("""() => {
      const state = window.rehearsalTest = {tabRequests: 0, microphoneRequests: 0};
      Object.defineProperty(navigator.mediaDevices, 'getDisplayMedia', {value: () => {
        state.tabRequests++;
        return Promise.reject(new DOMException('The chooser was dismissed.', 'NotAllowedError'));
      }});
      Object.defineProperty(navigator.mediaDevices, 'getUserMedia', {value: async () => {
        state.microphoneRequests++;
        const context = state.context = new AudioContext();
        const tone = context.createOscillator();
        const output = context.createMediaStreamDestination();
        tone.connect(output); tone.start();
        state.track = output.stream.getAudioTracks()[0];
        await context.resume();
        return output.stream;
      }});
    }""")


def exercise_rehearsal_audio(page):
    requests = []
    sockets = []
    capture_request = lambda request: requests.append(urlsplit(request.url).path)
    capture_socket = lambda ws: sockets.append(urlsplit(ws.url).path)
    page.on('request', capture_request)
    page.on('websocket', capture_socket)
    try:
        install_rehearsal_inputs(page)
        estimate = page.locator('#meet-start-estimate')
        expect(estimate).to_be_visible()
        expect(estimate).to_contain_text('US$0.00')
        button = page.locator('[data-rust-ui="audio-check"] #choose-meet-tab')
        expect(button).to_have_accessible_name('Start free rehearsal')
        expect(button).to_have_attribute('aria-describedby', 'meet-start-estimate')
        expect(button).to_be_visible()
        expect(button).to_be_disabled()
        expect(page.locator('#start-meet')).to_have_count(0)
        consent = page.get_by_label('Everyone agrees to this local audio check.')
        consent.check()
        expect(button).to_be_enabled()
        button.click()
        expect(page.get_by_role('alert').filter(has_text='cancelled or denied')).to_be_visible()
        page.locator('#meet-source').select_option('self')
        button.click()
        expect(page.locator('.meet-progress strong')).to_have_text('Microphone selected')
        page.wait_for_function("() => document.querySelector('#meet-audio-level').value > 0")
        expect(button).to_have_text('Release microphone')
        button.click()
        expect(page.get_by_text('Local audio check stopped.', exact=False)).to_be_visible()
        expect(button).to_have_text('Start free rehearsal')
        counts = page.evaluate("""async () => {
          const s = window.rehearsalTest;
          await s.context.close();
          return [s.tabRequests, s.microphoneRequests, s.track.readyState];
        }""")
        assert counts == [1, 1, 'ended']
        assert not sockets and '/api/routes' not in requests, 'Rehearsal attempted live admission'
    finally:
        page.remove_listener('request', capture_request)
        page.remove_listener('websocket', capture_socket)


def observe_operator_stream(page):
    # The native service performs the real handshake. Add fixture messages only
    # to this page's receive boundary, without reading its handshake protocols.
    page.evaluate("""() => {
      const NativeSocket = window.WebSocket;
      window.WebSocket = class extends NativeSocket {
        constructor(...args) {
          super(...args);
          if (new URL(this.url).pathname !== '/api/operator/events') return;
          this.addEventListener('message', event => {
            const hello = JSON.parse(event.data);
            if (hello.type !== 'hello' || hello.role !== 'operator') return;
            window.sendCaptionFixture = events => {
              for (const data of events) {
                this.dispatchEvent(new MessageEvent('message', {
                  data: JSON.stringify({...data, generation: hello.generation})
                }));
              }
            };
          }, {once: true});
        }
      };
    }""")


def exercise_caption_reader(operator, viewer):
    reader = operator.locator('[data-rust-ui="reader"]').filter(has=operator.locator('#meet-caption-preview'))
    hold = reader.get_by_role('button', name='Hold text', exact=True)
    source = reader.locator('#meet-caption-preview .meet-preview-text')
    target = reader.locator('#meet-interpretation-preview .meet-preview-text')
    size = reader.get_by_label('Caption size')
    expect(hold).to_be_disabled()

    # Enter the ordinary key-free rehearsal so that switching on the manual
    # stream does not replace the guided reader. No real microphone is opened.
    install_rehearsal_inputs(operator)
    operator.get_by_label('Everyone agrees to this local audio check.').check()
    operator.locator('#meet-source').select_option('self')
    operator.locator('#choose-meet-tab').click()
    expect(operator.locator('.meet-progress strong')).to_have_text('Microphone selected')

    captions = {'route_id': 'reader_source', 'source': 'self', 'route': 'transcription'}
    interpretation = {'route_id': 'reader_target', 'source': 'self', 'route': 'live'}
    routes = [dict(lane, type='route', language='en', live_mode=mode, state='ready',
                   code=None, expires_in_seconds=60)
              for lane, mode in [(captions, None), (interpretation, 'interpretation')]]
    def deliver(events):
        operator.evaluate('(events) => window.sendCaptionFixture(events)', events)

    def words(number):
        return [dict(captions, type='transcript', provenance='source_asr', text=f'Fixture source {number}.',
                     final=True, item={'id': f'utterance_{number}', 'revision': 1, 'sequence': number}),
                dict(interpretation, type='transcript', provenance='live_output', text=f'Fixture target {number}.',
                     final=False, item=None)]

    deliver(routes + words(1))
    expect(source).to_have_text('Fixture source 1.')
    expect(target).to_have_text('Fixture target 1.')
    hold.focus()
    operator.keyboard.press('Space')
    resume = reader.get_by_role('button', name='Return to live text', exact=True)
    expect(resume).to_be_focused()
    deliver(words(2))
    expect(source).to_have_text('Fixture source 1.')
    expect(target).to_have_text('Fixture target 1.')

    # Underlying archival and local export continue receiving speech while held.
    note = "Fictional private note for rehearsal."
    operator.locator("#private-notes").fill(note)
    with operator.expect_download() as download:
        operator.get_by_role('button', name='Export private captions & notes (.txt)').click()
    exported = download.value.path().read_text()
    assert download.value.suggested_filename == 'private-call-pilot-notes.txt'
    assert note in exported and 'Fixture source 2.' in exported and 'Fixture target 2.' not in exported
    expect(operator.locator('#private-notes')).to_have_value(note)
    font_size = '(node) => parseFloat(getComputedStyle(node).fontSize)'
    initial_sizes = [node.evaluate(font_size) for node in (source, target)]
    size.focus()
    operator.keyboard.press('e')  # Native select typeahead for “Extra large”.
    expect(size).to_have_value('extra')
    for node, initial in zip((source, target), initial_sizes):
        assert node.evaluate(font_size) > initial, 'Caption size did not change rendered text'
    resume.focus()
    operator.keyboard.press('Enter')
    expect(hold).to_be_focused()
    expect(source).to_have_text('Fixture source 2.')
    expect(target).to_have_text('Fixture target 2.')

    # Leave a private snapshot held across the real Stop later in the test.
    operator.keyboard.press('Enter')
    expect(resume).to_be_visible()
    expect(viewer.locator('[data-rust-ui="reader"]')).to_have_count(0)
    expect(viewer.locator('body')).not_to_contain_text('Fixture source')
    expect(viewer.locator('body')).not_to_contain_text('Fixture target')


def exercise_browser(directory, *, operator_origin=ORIGIN, viewer_origin=PUBLIC_ORIGIN):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel='chrome', headless=True)
        try:
            context = browser.new_context()
            unexpected_hosts = set()
            wasm_origins = set()
            def check_module(response):
                url = urlsplit(response.url)
                if url.path.endswith('.wasm'):
                    assert response.ok and response.headers.get('content-type') == 'application/wasm'
                    policy = response.headers['content-security-policy'].split(';')
                    scripts = next(d.split()[1:] for d in policy if d.strip().startswith('script-src '))
                    assert set(scripts) == {"'self'", "'wasm-unsafe-eval'"}, 'Unexpected script permission'
                    wasm_origins.add(f'{url.scheme}://{url.netloc}')
            context.on('response', check_module)

            def local_requests_only(route):
                url = urlsplit(route.request.url)
                if url.scheme in ('data', 'blob') or (
                    url.scheme == 'http' and f'{url.scheme}://{url.netloc}' in (operator_origin, viewer_origin)
                ):
                    route.continue_()
                else:
                    unexpected_hosts.add(url.hostname)
                    route.abort()

            context.route('**/*', local_requests_only)
            operator = context.new_page()
            try:
                operator.goto(bootstrap_url(directory, origin=operator_origin))
            except BrowserError:
                raise RuntimeError('Local operator bootstrap navigation failed') from None
            expect(operator.locator('#private-notes')).to_be_visible()
            expect(operator).to_have_title('Enny · Meeting interpreter')
            expect(operator.locator('[data-rust-ui="session"] .brand')).to_contain_text('Enny')
            expect(operator.locator('[data-rust-ui="notes"] #private-notes')).to_be_visible()
            expect(operator).to_have_url(operator_origin + '/')
            exercise_rehearsal_audio(operator)
            assert not context.cookies(), 'Operator bootstrap issued an ambient cookie'

            # Reload before opening a stream. This should restore only this tab's
            # authenticated session, without reusing the consumed entry ticket.
            operator.reload()
            expect(operator.locator('#private-notes')).to_be_visible()
            separate_tab = context.new_page()
            separate_tab.goto(operator_origin)
            expect(separate_tab.get_by_text('No valid local session.', exact=False)).to_be_visible()
            separate_tab.close()

            operator.get_by_text('Cost & limits', exact=True).click()
            minutes = operator.locator('[data-rust-ui="input"] #cost-minutes')
            minutes.fill('121')
            expect(minutes).to_have_attribute('aria-invalid', 'true')
            minutes.fill('17')
            expect(minutes).to_have_attribute('aria-invalid', 'false')
            operator.get_by_text('Advanced audio & manual routes', exact=True).click()
            observe_operator_stream(operator)
            operator.get_by_role('button', name='Connect private stream', exact=True).click()
            expect(operator.locator('.connection')).to_contain_text('Private stream connected')
            operator.get_by_text('Public presentation', exact=True).click()
            operator.get_by_role('button', name='Prepare public-screen link').click()
            with operator.expect_popup() as popup:
                operator.get_by_role('button', name='Open one-use public screen').click()
            viewer = popup.value
            expect(viewer.locator('[data-rust-ui="viewer"]').get_by_role('heading', name='Ready when you approve.')).to_be_visible()
            expect(viewer).to_have_url(viewer_origin + '/public')
            viewer.get_by_role('button', name='Enable this public screen & sound').click()
            expect(viewer.get_by_text('Viewer connected', exact=True)).to_be_visible()
            expect(viewer.locator('#private-notes')).to_have_count(0)
            expect(viewer.locator('[data-rust-ui="stop-receipt"]')).to_have_count(0)
            exercise_caption_reader(operator, viewer)
            assert not context.cookies(), 'Viewer bootstrap issued an ambient cookie'
            with viewer.expect_response(lambda response: response.url == viewer_origin + '/api/public/logout') as viewer_logout:
                viewer.get_by_role('button', name='Sign out').click()
            assert viewer_logout.value.ok, 'Viewer logout was not confirmed'
            viewer.reload()
            expect(viewer.get_by_text('No valid local session.', exact=False)).to_be_visible()
            with operator.expect_response(lambda response: response.url == operator_origin + '/api/stop-receipt') as receipt:
                with operator.expect_response(lambda response: response.url == operator_origin + '/api/stop') as stop:
                    operator.get_by_role('button', name='■ Stop pilot', exact=True).click()
            assert stop.value.ok, 'Caption reader lifecycle Stop was not acknowledged'
            assert receipt.value.ok
            terminal = stop.value.json()['generation']
            assert receipt.value.json() == {
                'version': 1, 'session_generation': terminal - 1, 'generation': terminal,
                'provider_activity': 'none_admitted', 'reserved_microusd': 0, 'invoice': 'unavailable',
            }
            panel = operator.locator('[data-rust-ui="stop-receipt"]')
            expect(panel).to_be_visible()
            for field in ('Browser audio', 'Server Stop', 'Provider transport', 'Local reservation', 'Provider invoice'):
                expect(panel.get_by_text(field, exact=True)).to_be_visible()
            expect(operator.locator('body')).not_to_contain_text('Fixture source')
            expect(operator.locator('body')).not_to_contain_text('Fixture target')
            expect(operator.locator('[data-rust-ui="reader"]')).to_have_count(0)
            with operator.expect_response(lambda response: response.url == operator_origin + '/api/logout') as operator_logout:
                operator.get_by_role('button', name='Sign out').click()
            assert operator_logout.value.ok, 'Operator logout was not confirmed'
            expect(operator.locator('[data-rust-ui="stop-receipt"]')).to_have_count(0)
            operator.reload()
            expect(operator.get_by_text('No valid local session.', exact=False)).to_be_visible()
            assert wasm_origins == {operator_origin, viewer_origin}, 'Missing Rust UI module on either origin'
            assert not unexpected_hosts, 'The key-free page tried to contact another host'
        finally:
            browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, choices=(5080, 5180), default=5180)
    port = parser.parse_args().port
    operator_origin, viewer_origin = port_pair(port)
    with ownership(port), tempfile.TemporaryDirectory(dir=local_dir()) as temp:
        directory = Path(temp)
        binary, config = prepare(directory, 120, port=port)
        process = subprocess.Popen([str(binary), '--config', str(config)], cwd=directory,
                                   env={'PATH': '/usr/bin:/bin', 'LANG': 'C'},
                                   stdin=subprocess.DEVNULL)
        try:
            wait_ready(process, directory, providers_enabled=False, origin=operator_origin)
            config.unlink()
            exercise_browser(directory, operator_origin=operator_origin, viewer_origin=viewer_origin)
        finally:
            stop_owned(process)
    print('Passed Rust/WASM caption reading, local text export, operator/viewer isolation, Stop and sign-out.')


if __name__ == '__main__':
    main()
