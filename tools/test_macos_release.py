"""Extract and exercise the actual macOS review ZIP without a provider key."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from build_macos_release import APP, ARCHIVE
from local_call_pilot import wait_ready


def check_rehearsal(app, root, env, chrome):
    with tempfile.TemporaryDirectory(dir=root) as temp:
        directory = Path(temp)
        credentials = directory / "credentials.json"
        config = {"assets_dir": str(app / "Contents/Resources/web"), "duration_seconds": 600,
                  "max_input_audio_seconds": 1200, "provider": None, "operator_port": 5180,
                  "credentials_path": str(credentials)}
        wire = json.dumps(config).encode("utf-8")
        with subprocess.Popen([app / "Contents/Helpers/meeting-call-pilot", "--desktop-pipe"],
                              stdin=subprocess.PIPE, cwd=directory, env=env) as child:
            try:
                child.stdin.write(struct.pack(">I", len(wire)) + wire)
                child.stdin.flush()
                wait_ready(child, directory, providers_enabled=False)
                assert credentials.stat().st_mode & 0o777 == 0o600
                if chrome:
                    from test_browser_session import exercise_browser
                    exercise_browser(directory)
            finally:
                # Simulate the owning launcher disappearing, including with
                # browser sessions open. No signal or API Stop is required.
                child.stdin.close()
                try:
                    child.wait(timeout=38)
                except subprocess.TimeoutExpired:
                    child.kill()
                    raise
            if child.returncode:
                raise RuntimeError("Backend did not shut down cleanly")


def check_download(download, chrome):
    manifest = download.with_suffix(".zip.sha256")
    if not manifest.exists():
        manifest = download.parent / "SHA256SUMS"
    entries = [line.split() for line in manifest.read_text().splitlines()]
    checksums = [entry[0] for entry in entries if len(entry) == 2 and entry[1] == ARCHIVE]
    if checksums != [hashlib.sha256(download.read_bytes()).hexdigest()]:
        raise ValueError("Download checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="interpreter-package-") as unpacked:
        root = Path(unpacked).resolve()
        app = root / APP
        subprocess.run(["/usr/bin/ditto", "-x", "-k", download, root], check=True)
        subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app], check=True)
        env = {"PATH": "/usr/bin:/bin", "LANG": "C"}
        launcher = app / "Contents/MacOS/Interpreter"
        subprocess.run([launcher, "--check"], env=env, check=True, timeout=55)
        if chrome:
            # Use the checkout's native form for keyboard/visual checks. The
            # extracted, signed launcher runs its own settings checks below.
            source = Path(__file__).resolve().parent / "macos/test_consent.m"
            probe = root / "check-consent"
            subprocess.run(["/usr/bin/clang", "-fobjc-arc", "-Werror", "-Wno-deprecated-declarations",
                            "-framework", "Cocoa", source, "-o", probe], check=True)
            subprocess.run([probe], check=True, timeout=15)
        check_rehearsal(app, root, env, chrome)
        # Follow the approval form's parser and launch the packaged service.
        # No browser opens and no audio route is admitted with the inert key.
        for minutes, budget, estimate, limit in [
            ("1", "0.07", "US$0.07", "US$0.07"), ("45", "3.02", "US$3.02", "US$3.02"),
            ("60", "4.02", "US$4.02", "US$4.02"), ("60", "1000", "US$4.02", "US$1000.00")
        ]:
            result = subprocess.run([launcher, "--check-live-config", minutes, budget],
                                    env=env, check=True, capture_output=True, text=True, timeout=55)
            quote = f"Estimated OpenAI cost for one listener is {estimate} for {minutes} minute"
            if quote not in result.stdout or f"Local limit: {limit}." not in result.stdout:
                raise RuntimeError("The launcher did not distinguish the OpenAI estimate from the local limit")
        for minutes, budget in [("0", "5"), ("61", "5"), ("1.5", "5"), ("60", "4.01"),
                                ("1", "0.06"), ("1", "1e2"), ("1", "0.070"), ("1", "1000.01")]:
            result = subprocess.run([launcher, "--check-live-config", minutes, budget],
                                    env=env, capture_output=True, timeout=5)
            if result.returncode != 2:
                raise RuntimeError("The launcher did not reject invalid meeting settings")
    print("Verified archive, code signature, native startup, rehearsal, selected BYOK limits and parent-pipe shutdown.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--chrome", action="store_true", help="also exercise the native consent window and Chrome operator/viewer login")
    args = parser.parse_args()
    check_download(args.archive.resolve(strict=True), args.chrome)
