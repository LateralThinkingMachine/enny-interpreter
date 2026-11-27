#!/usr/bin/env python3
"""Launch the standalone interpreter. The default session runs on loopback with providers disabled.
Use --open to launch the local browser entry. On Linux, select the private credentials file.
Live provider access requires an explicitly prepared, fresh activation profile.
"""
import argparse
from contextlib import contextmanager
import fcntl
import http.client
import io
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]


def local_dir():
    path = ROOT / '.interpreter-runtime'
    path.mkdir(mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError('Expected a private owned runtime directory')
    return path


ORIGIN = 'http://127.0.0.1:5180'
PUBLIC_ORIGIN = 'http://127.0.0.1:5182'


def port_pair(port=5180):
    """Reviewed loopback pairs only; a port never grants provider authority."""
    if type(port) is not int or port not in (5080, 5180):
        raise ValueError('Unsupported pilot port')
    return f'http://127.0.0.1:{port}', f'http://127.0.0.1:{port + 2}'


def private_json(path, value):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as output:
        json.dump(value, output, separators=(',', ':'))
        output.flush()
        os.fsync(output.fileno())


def read_private(path, limit=16384):
    path = Path(path)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError('A direct absolute private path is required')
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > limit:
            raise ValueError('Private file permissions or size invalid')
        content = source.read(limit + 1)
    if len(content) > limit:
        raise ValueError('Private file too large')
    return json.loads(content)


@contextmanager
def ownership(port=5180):
    port_pair(port)
    lock = local_dir() / 'call-pilot-launch.lock'
    with os.fdopen(os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600), 'r+b') as output:
        info = os.fstat(output.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
            raise RuntimeError('Invalid pilot ownership lock')
        try:
            fcntl.flock(output, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another owned pilot launch is running') from None
        for listener_port in (port, port + 2):
            with socket.socket() as probe:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                try:
                    probe.bind(('127.0.0.1', listener_port))
                except OSError:
                    raise RuntimeError('Pilot port is occupied; existing process left untouched') from None
        yield


def prepare(directory, duration_seconds, *, port=5180):
    port_pair(port)
    directory = Path(directory)
    if ((duration_seconds is not None and (type(duration_seconds) is not int or not 1 <= duration_seconds <= 7200))
            or directory.resolve(strict=True) != directory):
        raise ValueError('Invalid pilot limits or directory')
    info = directory.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError('Private owned directory required')
    assets = ROOT / 'apps/call-pilot/dist'
    executable = ROOT / 'target/debug/meeting-call-pilot'
    if not (assets / 'index.html').is_file() or not executable.is_file():
        raise RuntimeError('Build the pilot frontend and Rust executable first')
    # Capture one immutable executable inode, independent of later agent builds.
    target = directory / 'meeting-call-pilot'
    with os.fdopen(os.open(executable, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or not info.st_mode & 0o111:
            raise RuntimeError('Expected owned pilot executable')
        with os.fdopen(os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o700), 'wb') as destination:
            shutil.copyfileobj(source, destination)
            destination.flush()
            os.fsync(destination.fileno())
    # Copy a completed, quiescent frontend build before serving it. Callers must
    # wait for the build to finish; this is not an atomic snapshot of a concurrent
    # Vite build. Later source/build changes cannot alter these served copies.
    frozen_assets = directory / 'assets'
    frozen_assets.mkdir(mode=0o700)
    for path in sorted(assets.rglob('*')):
        if path.is_symlink():
            raise ValueError('Symlink asset rejected')
        relative = path.relative_to(assets)
        if path.is_dir():
            (frozen_assets / relative).mkdir(mode=0o700)
        elif path.is_file():
            if path.stat().st_size > 8 * 1024 * 1024:
                raise ValueError('Asset too large')
            shutil.copyfile(path, frozen_assets / relative, follow_symlinks=False)
        else:
            raise ValueError('Non-regular asset rejected')
    config = directory / 'config.json'
    private_json(config, {
        'assets_dir': str(frozen_assets),
        'credentials_path': str(directory / 'credentials.json'),
        'duration_seconds': duration_seconds,
        'max_input_audio_seconds': 1200 if duration_seconds is None else duration_seconds * 2,
        'operator_port': port,
        'provider': None,
    })
    return target, config


def _readiness_remaining(process, deadline):
    if process.poll() is not None:
        raise RuntimeError('Pilot process failed before readiness; inspect its private log')
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError('Pilot readiness deadline')
    return remaining


def _read_health(process, deadline, *, origin=None):
    """Fixed loopback HTTP only; bounded wire bytes before in-memory parsing.

    Connection: close is supported by the owned Axum health endpoint. A peer
    that drips headers/body or never closes cannot extend this attempt's total
    deadline. No redirect, proxy, DNS hostname, cookie or credential is used.
    """
    origin = urlsplit(ORIGIN if origin is None else origin)
    port = origin.port
    if (origin.scheme != 'http' or origin.hostname != '127.0.0.1'
            or port is None or origin.netloc != f'127.0.0.1:{port}'
            or origin.path or origin.query or origin.fragment):
        raise ValueError('Invalid fixed pilot readiness origin')
    request = (f'GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
               'Accept: application/json\r\nConnection: close\r\n\r\n').encode('ascii')
    wire = bytearray()
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
        connection.settimeout(min(.1, _readiness_remaining(process, deadline)))
        connection.connect(('127.0.0.1', port))
        connection.settimeout(min(.1, _readiness_remaining(process, deadline)))
        connection.sendall(request)
        while True:
            connection.settimeout(min(.1, _readiness_remaining(process, deadline)))
            try:
                chunk = connection.recv(min(4096, 16385 - len(wire)))
            except socket.timeout:
                continue
            if not chunk:
                break
            wire.extend(chunk)
            if len(wire) > 16384:
                raise ValueError('Pilot readiness wire bound exceeded')
    _readiness_remaining(process, deadline)
    # The standard HTTP parser runs only over finite memory, so its header and
    # chunked-body parsing cannot introduce a separate blocking read deadline.
    class ResponseSocket:
        def makefile(self, *_args):
            return io.BytesIO(wire)
    with http.client.HTTPResponse(ResponseSocket()) as response:
        response.begin()
        if response.status != 200:
            return None
        body = response.read(4097)
        if len(body) > 4096 or response.length not in (None, 0):
            raise ValueError('Pilot readiness body bound exceeded')
        state = json.loads(body)
    _readiness_remaining(process, deadline)
    return state


def wait_ready(process, directory, timeout=15, *, providers_enabled=False, origin=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError('Pilot process failed before readiness; inspect its private log')
        if (directory / 'credentials.json').exists():
            try:
                state = _read_health(process, min(deadline, time.monotonic() + 1), origin=origin)
                if isinstance(state, dict) and state.get('mode') == 'supervised_local_pilot' and state.get('providers_enabled') is providers_enabled:
                    return
            except (OSError, ValueError, http.client.HTTPException):
                pass
        time.sleep(min(.05, max(0, deadline - time.monotonic())))
    raise RuntimeError('Pilot readiness timed out')


def bootstrap_url(directory, *, origin=None):
    origin = ORIGIN if origin is None else origin
    if origin not in (port_pair(5080)[0], port_pair(5180)[0]):
        raise ValueError('Unsupported bootstrap origin')
    value = read_private(Path(directory) / 'credentials.json')
    ticket = value.get('bootstrap')
    if value.get('origin') != origin or not isinstance(ticket, str) or len(ticket) != 64 or any(c not in '0123456789abcdef' for c in ticket):
        raise ValueError('Invalid local bootstrap capability')
    return origin + '/#bootstrap=' + ticket


def open_chrome(directory, *, origin=None):
    # Transfer the one-use local capability through private stdin, not argv/logs.
    script = 'tell application "Google Chrome"\nopen location ' + json.dumps(bootstrap_url(directory, origin=origin)) + '\nactivate\nend tell\n'
    result = subprocess.run(['/usr/bin/osascript', '-'], input=script, text=True, capture_output=True, timeout=10)
    if result.returncode:
        raise RuntimeError('Chrome opening failed; local capability remains in the private run directory')


def browser_entry(directory, *, open_browser, origin):
    if open_browser and sys.platform == 'darwin':
        open_chrome(directory, origin=origin)
        return
    helper = ROOT / 'tools/open-operator.html'
    if open_browser and sys.platform.startswith('linux') and (os.environ.get('DISPLAY') or os.environ.get('WAYLAND_DISPLAY')):
        # Explicit Linux executables keep WSL users in the service's network environment.
        for browser in ('/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser', '/snap/bin/chromium'):
            if not os.access(browser, os.X_OK):
                continue
            # Pass only the public helper path. The operator selects the private file in the browser.
            gui_env = {name: os.environ[name] for name in (
                'HOME', 'USER', 'LANG', 'DISPLAY', 'WAYLAND_DISPLAY', 'XDG_RUNTIME_DIR',
                'XDG_SESSION_TYPE', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS',
                'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'PULSE_SERVER') if name in os.environ}
            try:
                subprocess.Popen([browser, helper.as_uri()], env={**gui_env, 'PATH': '/usr/local/bin:/usr/bin:/bin'},
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 start_new_session=True)
                break
            except OSError:
                continue
    print('Your local browser entry file is ' + str(helper) + '. Open it in a browser on this host.', flush=True)
    print('Select this file in the browser within two minutes: ' + str(directory / 'credentials.json'), flush=True)
    if sys.platform.startswith('linux'):
        print('On WSL, use a Linux browser running inside the same distribution.', flush=True)


def stop_owned(process, *, providers_enabled=False):
    try:
        result = process.poll()
        if result is None:
            process.terminate()
            try:
                # Rust allows 32s for provider closure and 3s for listener retirement.
                # Do not kill an owned in-flight WebRTC creation before it can be closed.
                result = process.wait(timeout=35)
            except subprocess.TimeoutExpired:
                process.kill()
                result = process.wait(timeout=5)
        if providers_enabled and result != 0:
            raise RuntimeError('Provider shutdown was not confirmed')
    except (OSError, subprocess.SubprocessError, RuntimeError):
        if providers_enabled:
            print('Provider shutdown could not be confirmed. Check your provider account for active sessions and usage.', file=sys.stderr)
        raise


def run(minutes=110, open_browser=False, activation_profile=None, *, port=5180, continuous=False):
    from call_pilot_activation import activate, read_profile
    origin, _ = port_pair(port)
    profile = read_profile(activation_profile) if activation_profile else None
    if profile:
        continuous = profile['version'] == 4
        if not continuous:
            minutes = profile['limits']['total_duration_seconds'] // 60
    with ownership(port), tempfile.TemporaryDirectory(prefix='session-', dir=local_dir()) as temporary:
        directory = Path(temporary)
        executable, config = prepare(directory, None if continuous else minutes * 60, port=port)
        process = None
        previous = {}
        stopping = False
        launching = False
        interrupted = False

        def request_stop(_number, _frame):
            nonlocal interrupted, stopping
            interrupted = True
            if not launching and not stopping:
                stopping = True
                raise KeyboardInterrupt

        def check_stop():
            if interrupted:
                raise KeyboardInterrupt

        try:
            for number in (signal.SIGINT, signal.SIGTERM):
                previous[number] = signal.signal(number, request_stop)
            if profile:
                config = activate(activation_profile, executable=executable, config=config,
                                  stop_check=check_stop, expected_profile=profile)
            launching = True
            try:
                check_stop()
                process = subprocess.Popen([str(executable), '--config', str(config)],
                                           cwd=directory, env={'PATH': '/usr/bin:/bin', 'LANG': 'C'},
                                           stdin=subprocess.DEVNULL)
            finally:
                launching = False
            check_stop()
            wait_ready(process, directory, providers_enabled=profile is not None, origin=origin)
            config.unlink()
            print('Interpreter ready at ' + origin + '. Providers ' +
                  ('enabled by the selected profile.' if profile else 'disabled.'), flush=True)
            browser_entry(directory, open_browser=open_browser, origin=origin)
            print('Keep this terminal running. Press Ctrl-C to stop.', flush=True)
            result = process.wait(timeout=None if continuous else minutes * 60 + 40)
            if result:
                raise RuntimeError('Interpreter exited unexpectedly')
        except KeyboardInterrupt:
            print('Stopping the interpreter.', flush=True)
        finally:
            stopping = True
            try:
                if process is not None:
                    stop_owned(process, providers_enabled=profile is not None)
            finally:
                for number, handler in previous.items():
                    signal.signal(number, handler)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, choices=(5080, 5180), default=5180, help='Operator loopback port; separate public listener uses this port + 2.')
    parser.add_argument('--duration-minutes', type=int)
    parser.add_argument('--continuous', action='store_true', help='Run until stopped; alone starts a provider-disabled local process. A continuous profile requires explicit warning-only spending authority.')
    parser.add_argument('--warning-usd', help='Explicit warning threshold for a new continuous activation. This is not a spending ceiling.')
    parser.add_argument('--open', action='store_true', help='Open the local browser entry on macOS or desktop Linux; on Linux, choose the private credentials file when prompted')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--prepare-activation', metavar='ABSOLUTE_PROFILE', help='Create a private key-free profile; does not enable providers')
    mode.add_argument('--activate', metavar='ABSOLUTE_PROFILE', help='Consume a fresh profile reservation, then load only the designated project .env')
    parser.add_argument('--provider', choices=('openai', 'gemini'), help='Select the destination when preparing a finite profile. Existing profiles use their recorded provider.')
    parser.add_argument('--budget-usd', help='Explicit finite allowance for a new activation; never inherited from a rehearsal')
    parser.add_argument('--routes', help='Explicit comma-separated transcription,translation,live subset')
    parser.add_argument('--live-modes', help='Required with live: explicit comma-separated assistant,interpretation subset; no automatic mode switching')
    parser.add_argument('--max-sessions', type=int, help='Explicit provider stream admission limit, 1–16; defaults to 16. Each reopen consumes another admission.')
    parser.add_argument('--tts-requests', type=int, help='Explicit allowance of 1–8 prepared TTS clips; omitted means disabled. Shares the recorded USD budget.')
    for name in ('participant-consent', 'capture-routing-checked', 'headphones-checked', 'quality-limitations-reviewed'):
        parser.add_argument('--' + name, action='store_true', help='Record this operator attestation; not an automated check')
    args = parser.parse_args(argv)
    if args.provider is not None and (not args.prepare_activation or args.continuous):
        parser.error('Select the provider only when preparing a finite activation')
    if args.continuous and args.duration_minutes is not None:
        parser.error('Continuous mode has no process duration override')
    if args.activate and (args.continuous or args.warning_usd is not None):
        parser.error('Activation uses its exact recorded lifetime and spending policy')
    if args.warning_usd is not None and not (args.continuous and args.prepare_activation):
        parser.error('A warning threshold belongs only to explicit continuous profile preparation')
    if args.continuous and any((args.budget_usd is not None, args.routes is not None,
                               args.live_modes is not None, args.max_sessions is not None, args.tts_requests is not None)):
        parser.error('Continuous mode has a fixed captions/interpreter scope; legacy budget, routes and admission overrides are refused')
    if args.duration_minutes is not None and not 1 <= args.duration_minutes <= 120:
        parser.error('Duration must be between 1 and 120 minutes')
    if args.tts_requests is not None and not 1 <= args.tts_requests <= 8:
        parser.error('Prepared TTS allowance must be between 1 and 8 requests')
    if args.max_sessions is not None and not 1 <= args.max_sessions <= 16:
        parser.error('Provider admission limit must be between 1 and 16')
    if args.activate and any((args.duration_minutes, args.budget_usd, args.routes, args.tts_requests, args.live_modes is not None, args.max_sessions)):
        parser.error('Activation uses its exact recorded scope; no command-line override')
    if not args.prepare_activation and any((args.budget_usd, args.routes, args.tts_requests, args.live_modes is not None, args.max_sessions, args.participant_consent, args.capture_routing_checked, args.headphones_checked, args.quality_limitations_reviewed)):
        parser.error('Budget, routes and attestations belong only to explicit profile preparation')
    try:
        if args.prepare_activation:
            if args.open or (args.continuous and args.warning_usd is None) or (not args.continuous and (args.duration_minutes is None or args.budget_usd is None or args.routes is None)):
                parser.error('Profile preparation requires budget, duration and routes and never opens a browser')
            from call_pilot_activation import prepare_profile, prepare_continuous_profile
            # Same offline build snapshot path as runtime; no provider/key code.
            with tempfile.TemporaryDirectory(prefix='call-pilot-profile-', dir=local_dir()) as temporary:
                binary, config = prepare(Path(temporary), None if args.continuous else args.duration_minutes * 60, port=args.port)
                current = read_private(config)
                attestations = {'participant_consent': args.participant_consent, 'capture_routing_checked': args.capture_routing_checked,
                                'headphones_checked': args.headphones_checked, 'quality_limitations_reviewed': args.quality_limitations_reviewed}
                if args.continuous:
                    profile = prepare_continuous_profile(Path(args.prepare_activation), executable=binary,
                        assets=Path(current['assets_dir']), warning=args.warning_usd, attestations=attestations)
                else:
                    profile = prepare_profile(Path(args.prepare_activation), executable=binary, assets=Path(current['assets_dir']),
                        duration_seconds=args.duration_minutes * 60, budget=args.budget_usd, routes=args.routes.split(','), tts_requests=args.tts_requests or 0,
                        live_modes=args.live_modes.split(',') if args.live_modes is not None else None,
                        max_sessions=args.max_sessions if args.max_sessions is not None else 16, attestations=attestations,
                        vendor=args.provider or 'openai')
            print(json.dumps({'status': 'key_free_profile_prepared', 'activation_id': profile['activation_id'],
                              'expires_unix': profile['expires_unix'], 'limits': profile['limits'],
                              'provider': profile.get('vendor', 'openai'), 'key_accessed': False, 'providers_enabled': False, 'attestations_are_operator_declarations': True}))
        elif args.activate:
            run(open_browser=args.open, activation_profile=Path(args.activate), port=args.port)
        else:
            run(args.duration_minutes or 110, args.open, port=args.port,
                **({'continuous': True} if args.continuous else {}))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        raise SystemExit('Pilot launcher failed. No credentials or raw process diagnostics are printed.') from None


if __name__ == '__main__':
    main()
