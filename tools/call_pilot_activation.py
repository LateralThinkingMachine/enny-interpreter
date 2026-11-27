"""Explicit operator activation profiles. No key access during profile preparation.

This allowance is independent of every synthetic rehearsal. Operator attestations
are recorded declarations, not automated proof of language or device quality.
"""
from decimal import Decimal
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]

CHECKS = ('participant_consent', 'capture_routing_checked', 'headphones_checked', 'quality_limitations_reviewed')
ROUTES = ('transcription', 'translation', 'live')
LIVE_MODES = ('assistant', 'interpretation')
KEY_NAMES = {'openai': 'OPENAI_API_KEY', 'gemini': 'GEMINI_API_KEY'}
GEMINI_MAX_DURATION_SECONDS = 600


def validate_gemini_scope(routes, live_modes, tts_requests):
    if set(routes) != {'transcription', 'live'} or len(routes) != 2 or live_modes != ['interpretation'] or tts_requests != 0:
        raise ValueError('Gemini requires source captions and fixed-target interpretation with no prepared speech')
# Version 3 text limits bound provider text ingress, not unique speech, tokens or
# billed usage. Unicode scalar values match Rust's str::chars accounting, so
# combining marks count separately. 64 scalars/second/representation is explicit
# engineering headroom, not a measured language-quality or speech-rate claim.
TEXT_SCALARS_PER_SECOND = 64
TEXT_REPRESENTATIONS = {'transcription': 2, 'translation': 1, 'live': 2}
MAX_TEXT_SCALARS = 1_000_000
MAX_TTS_TEXT_SCALARS = 2000  # The existing 2000 UTF-8-byte request cap is tighter.


def continuous_limits_for(warning_microusd):
    """Version 4: explicit ongoing operation, finite bounds for each stream.

    The warning threshold is not an allowance or a lifetime spending ceiling.
    Changing this reviewed shape requires a new version, never reinterpretation
    of a consumed bounded profile.
    """
    if type(warning_microusd) is not int or not 10_000 <= warning_microusd <= 1_000_000_000:
        raise ValueError('Invalid continuous warning threshold')
    return {'session_policy': 'until_stopped', 'budget_policy': 'warning_only',
            'warning_threshold_microusd': warning_microusd,
            'allowed_routes': ['transcription', 'live'], 'allowed_live_modes': ['interpretation'],
            'route_duration_seconds': 300, 'max_concurrent_sessions': 2,
            'max_input_audio_seconds': 600, 'max_output_audio_seconds': 600,
            'max_text_characters': 76800}


def _directory(path):
    path = Path(path)
    info = path.lstat()
    if (not path.is_absolute() or path.resolve(strict=True) != path or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError('Private owned activation directory required')
    return path


def _sync(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _write(path, value):
    path = Path(path); _directory(path.parent)
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as output:
        json.dump(value, output, sort_keys=True, separators=(',', ':'))
        output.flush(); os.fsync(output.fileno())
    _sync(path.parent)


def _bytes(path, maximum, private=False):
    path = Path(path)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError('Direct activation input required')
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or not 0 < info.st_size <= maximum
                or (private and stat.S_IMODE(info.st_mode) != 0o600)):
            raise ValueError('Invalid activation input')
        value = source.read(maximum + 1)
    if len(value) > maximum:
        raise ValueError('Activation input too large')
    return value


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate activation field')
        result[key] = value
    return result


def money(value):
    if not isinstance(value, str) or not re.fullmatch(r'(?:0|[1-9][0-9]{0,3})(?:\.[0-9]{1,6})?', value):
        raise ValueError('Budget must be an explicit USD decimal')
    amount = int(Decimal(value) * 1_000_000)
    if not 10_000 <= amount <= 1_000_000_000:
        raise ValueError('Budget must be between USD0.01 and USD1000')
    return amount


def limits_for(duration_seconds, budget_microusd, routes, tts_requests=0, *, live_modes=None, max_sessions=16, version=3):
    if version == 5 and type(duration_seconds) is int and duration_seconds > GEMINI_MAX_DURATION_SECONDS:
        raise ValueError('Gemini source preview is limited to 10 minutes')
    if (type(duration_seconds) is not int or not 60 <= duration_seconds <= 7200 or duration_seconds % 60
            or type(budget_microusd) is not int or not 10_000 <= budget_microusd <= 1_000_000_000
            or not isinstance(routes, list) or not routes or len(routes) > 3
            or any(not isinstance(route, str) or route not in ROUTES for route in routes) or len(set(routes)) != len(routes)
            or type(tts_requests) is not int or not 0 <= tts_requests <= 8
            or type(version) is not int or version not in (1, 2, 3, 5)
            or type(max_sessions) is not int or not 1 <= max_sessions <= 16):
        raise ValueError('Invalid activation limits')
    if version == 1:
        # Historical profiles have exactly their original shape and assistant
        # behavior. They can never acquire interpreter authority by default.
        if live_modes is not None or max_sessions != 16:
            raise ValueError('Legacy activation scope changed')
    else:
        if live_modes is None and 'live' not in routes:
            live_modes = []
        if (not isinstance(live_modes, list) or len(live_modes) > 2
                or any(not isinstance(mode, str) or mode not in LIVE_MODES for mode in live_modes)
                or len(set(live_modes)) != len(live_modes)
                or bool(live_modes) != ('live' in routes)):
            raise ValueError('Explicit compatible Live modes required')
    # Legacy profiles keep their original exact resource limits. Never upgrade
    # their approval by recomputing them with a newer version's formula.
    text_limit = min(MAX_TEXT_SCALARS, max(2000, duration_seconds * 4))
    if version in (3, 5):
        # ASR charges deltas AND the final transcript; Live charges input AND
        # output transcripts. The admission pool has no per-route allocation,
        # so any admission may use the largest allowed representation count.
        # The 1m global ceiling remains finite even for long/high-fanout scopes:
        # it does not guarantee every hypothetical dense session will fit.
        per_admission = min(3600, duration_seconds) * TEXT_SCALARS_PER_SECOND * max(TEXT_REPRESENTATIONS[route] for route in routes)
        text_limit = min(MAX_TEXT_SCALARS, per_admission * max_sessions + tts_requests * MAX_TTS_TEXT_SCALARS)
    # Finite fan-out bounds; each route's copied input consumes provider budget.
    limits = {'allowed_routes': routes, 'allow_tts': tts_requests > 0, 'total_duration_seconds': duration_seconds,
            'route_duration_seconds': min(3600, duration_seconds), 'max_sessions': max_sessions, 'max_tts_requests': tts_requests,
            'max_input_audio_seconds': duration_seconds * len(routes) * 2,
            'max_output_audio_seconds': max(1, duration_seconds * len([r for r in routes if r != 'transcription']) * 2 + tts_requests * 60),
            'max_text_characters': text_limit,
            'approved_budget_microusd': budget_microusd}
    if version >= 2:
        limits['allowed_live_modes'] = list(live_modes)
    return limits


def build_identity(executable, assets):
    executable = Path(executable); assets = Path(assets)
    if not executable.stat().st_mode & 0o111 or assets.resolve(strict=True) != assets:
        raise ValueError('Invalid activation build')
    files = sorted(assets.rglob('*'))
    if len(files) > 128:
        raise ValueError('Activation asset bound')
    inventory = {}; total = 0
    for path in files:
        if path.is_symlink():
            raise ValueError('Linked activation asset')
        if path.is_dir():
            continue
        content = _bytes(path, 8 * 1024 * 1024)
        total += len(content)
        if total > 32 * 1024 * 1024:
            raise ValueError('Activation asset bound')
        inventory[str(path.relative_to(assets))] = hashlib.sha256(content).hexdigest()
    if 'index.html' not in inventory:
        raise ValueError('Missing activation application')
    return {'executable_sha256': hashlib.sha256(_bytes(executable, 256 * 1024 * 1024)).hexdigest(), 'assets_sha256': inventory}


def prepare_profile(path, *, executable, assets, duration_seconds, budget, routes, attestations, tts_requests=0,
                    live_modes=None, max_sessions=16, vendor="openai"):
    if not isinstance(attestations, dict) or set(attestations) != set(CHECKS) or any(attestations[name] is not True for name in CHECKS):
        raise ValueError('All operator attestations are required')
    now = int(time.time())
    profile = {'version': 3, 'mode': 'supervised_local_live_activation', 'activation_id': 'live_' + uuid.uuid4().hex,
               'created_unix': now, 'expires_unix': now + 900,
               'limits': limits_for(duration_seconds, money(budget), routes, tts_requests,
                                    live_modes=live_modes, max_sessions=max_sessions,
                                    version=5 if vendor == 'gemini' else 3),
               'attestations': attestations, 'build': build_identity(executable, assets)}
    if vendor not in KEY_NAMES:
        raise ValueError('Unknown provider')
    if vendor == 'gemini':
        validate_gemini_scope(routes, live_modes, tts_requests)
        profile.update(version=5, vendor=vendor)
    _write(path, profile)
    return profile


def prepare_continuous_profile(path, *, executable, assets, warning, attestations):
    if not isinstance(attestations, dict) or set(attestations) != set(CHECKS) or any(attestations[name] is not True for name in CHECKS):
        raise ValueError('All operator attestations are required')
    now = int(time.time())
    profile = {'version': 4, 'mode': 'supervised_local_live_activation',
               'activation_id': 'live_' + uuid.uuid4().hex,
               'created_unix': now, 'expires_unix': now + 900,
               'limits': continuous_limits_for(money(warning)),
               'attestations': attestations, 'build': build_identity(executable, assets)}
    _write(path, profile)
    return profile


def read_profile(path):
    profile = json.loads(_bytes(path, 65536, private=True), object_pairs_hook=_object)
    keys = {'version', 'mode', 'activation_id', 'created_unix', 'expires_unix', 'limits', 'attestations', 'build'}
    if isinstance(profile, dict) and profile.get('version') == 5:
        keys.add('vendor')
    if (not isinstance(profile, dict) or set(profile) != keys
            or type(profile['version']) is not int or profile['version'] not in (1, 2, 3, 4, 5)
            or (profile['version'] == 5 and profile['vendor'] != 'gemini')
            or profile['mode'] != 'supervised_local_live_activation'
            or not isinstance(profile['activation_id'], str) or not re.fullmatch(r'live_[0-9a-f]{32}', profile['activation_id'])):
        raise ValueError('Invalid activation profile')
    now = time.time()
    if (type(profile['created_unix']) is not int or type(profile['expires_unix']) is not int
            or not now - 900 < profile['created_unix'] <= now + 5
            or profile['expires_unix'] != profile['created_unix'] + 900 or profile['expires_unix'] <= now):
        raise ValueError('Activation profile expired')
    attestation = profile['attestations']; limits = profile['limits']
    if profile['version'] == 4:
        numeric = ('warning_threshold_microusd', 'route_duration_seconds', 'max_concurrent_sessions',
                   'max_input_audio_seconds', 'max_output_audio_seconds', 'max_text_characters')
        if (not isinstance(attestation, dict) or set(attestation) != set(CHECKS)
                or any(attestation[name] is not True for name in CHECKS)
                or not isinstance(limits, dict)
                or limits != continuous_limits_for(limits.get('warning_threshold_microusd'))
                or any(type(limits[name]) is not int for name in numeric)):
            raise ValueError('Continuous activation scope changed')
        return profile
    if (not isinstance(attestation, dict) or set(attestation) != set(CHECKS) or any(attestation[name] is not True for name in CHECKS)
            or not isinstance(limits, dict) or limits != limits_for(limits.get('total_duration_seconds'), limits.get('approved_budget_microusd'), limits.get('allowed_routes'), limits.get('max_tts_requests'),
                live_modes=limits.get('allowed_live_modes') if profile['version'] >= 2 else None,
                max_sessions=limits.get('max_sessions') if profile['version'] >= 2 else 16,
                version=profile['version'])
            or any(type(limits[name]) is not int for name in limits if name not in ('allowed_routes', 'allowed_live_modes', 'allow_tts'))
            or type(limits['allow_tts']) is not bool):
        raise ValueError('Activation scope changed')
    if profile['version'] == 5:
        validate_gemini_scope(limits['allowed_routes'], limits['allowed_live_modes'], limits['max_tts_requests'])
    return profile


def read_approved_key(path, vendor="openai"):
    """Parse only the designated file without following links or blocking on a FIFO."""
    path = Path(path)
    if path.resolve(strict=True) != path:
        raise ValueError('invalid_approved_env_file')
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > 65536:
            raise ValueError('invalid_approved_env_file')
        data = source.read(65537)
    if len(data) > 65536:
        raise ValueError('approved_env_file_too_large')
    try:
        lines = data.decode('utf-8').splitlines()
    except UnicodeError:
        raise ValueError('invalid_approved_env_encoding') from None
    if vendor not in KEY_NAMES:
        raise ValueError('Unknown provider')
    name = KEY_NAMES[vendor]
    matches = []
    for line in lines:
        match = re.fullmatch(rf'\s*(?:export\s+)?{name}\s*=\s*(.*?)\s*', line)
        if match:
            value = match.group(1)
            if len(value) >= 2 and value[0] in ('"', "'") and value[-1] == value[0]:
                value = value[1:-1]
            if (not 16 <= len(value) <= 4096 or not value.isascii()
                    or any(ord(c) < 33 or ord(c) > 126 for c in value)
                    or any(c in value for c in '\\`$"\'')):
                raise ValueError('invalid_approved_key_format')
            if ((vendor == 'gemini' and value.startswith('sk-'))
                    or (vendor == 'openai' and value.startswith(('AIza', 'AQ.')))):
                raise ValueError('credential_family_mismatch')
            matches.append(value)
    if len(matches) != 1:
        raise ValueError('exactly_one_approved_key_required')
    return matches[0]


def activate(path, *, executable, config, stop_check, expected_profile=None):
    profile = read_profile(path)
    if expected_profile is not None and profile != expected_profile:
        raise ValueError('Activation profile changed since selection')
    config = Path(config)
    current = json.loads(_bytes(config, 65536, private=True), object_pairs_hook=_object)
    continuous = profile['version'] == 4
    lifetime_matches = (('duration_seconds' in current and current['duration_seconds'] is None
                         and type(current.get('max_input_audio_seconds')) is int
                         and current['max_input_audio_seconds'] == 1200
                         and current.get('provider') is None) if continuous
                        else current['duration_seconds'] == profile['limits']['total_duration_seconds'])
    if ('sales_mcp' in current or current.get('repeatable_sessions', False) is not False
            or profile['build'] != build_identity(executable, Path(current['assets_dir'])) or not lifetime_matches):
        raise ValueError('Activation build or duration changed')
    stop_check()
    if read_profile(path) != profile:
        raise ValueError('Activation profile changed during preparation')
    ledger = ROOT / '.call-pilot-live-approval-ledger'
    ledger.mkdir(mode=0o700, exist_ok=True); _directory(ledger); _sync(ledger.parent)
    # Never remove this reservation on error, Stop or provider usage uncertainty.
    digest = hashlib.sha256(json.dumps(profile, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    if continuous:
        record = {'activation_id': profile['activation_id'], 'profile_version': 4,
                  'session_policy': 'until_stopped', 'budget_policy': 'warning_only',
                  'warning_threshold_microusd': profile['limits']['warning_threshold_microusd'],
                  'profile_sha256': digest, 'consumed_unix': int(time.time()),
                  'planned_stream_renewal_allowed': True, 'automatic_process_restart_allowed': False,
                  'account_hard_cap_proven': False}
    else:
        record = {'activation_id': profile['activation_id'], 'reserved_microusd': profile['limits']['approved_budget_microusd'],
                  'profile_sha256': digest, 'reserved_unix': int(time.time()),
                  'automatic_retry_allowed': False, 'account_hard_cap_proven': False}
    _write(ledger / (profile['activation_id'] + '.json'), record)
    stop_check()
    key = None
    try:
        vendor = profile.get('vendor', 'openai')
        key = read_approved_key(ROOT / ('.env.gemini' if vendor == 'gemini' else '.env'), vendor)
        stop_check()
        if read_profile(path) != profile:
            raise ValueError('Activation profile changed during key loading')
        if continuous:
            current['provider'] = {'api_key': key, 'continuous': {
                name: value for name, value in profile['limits'].items()
                if name not in ('session_policy', 'budget_policy')}}
        else:
            current['provider'] = {**profile['limits'], 'api_key': key}
            if vendor == 'gemini':
                current['provider']['vendor'] = vendor
        target = config.parent / 'provider-config.json'
        _write(target, current)
        return target
    finally:
        key = None
        current.pop('provider', None)
