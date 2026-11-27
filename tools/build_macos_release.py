"""Build the private-review Apple Silicon download from the locked source tree."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import shlex
import subprocess
import sys
import tempfile
import zipfile

from binary_notices import write_desktop_notice_index

ROOT = Path(__file__).resolve().parents[1]
APP = "Real-time Interpreter.app"
ARCHIVE = "real-time-interpreter-macos-arm64.zip"
TARGET = "aarch64-apple-darwin"
MIN_MACOS = "14.0"


def run(*args, **kwargs):
    subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def build(offline):
    home = Path.home()
    # Build scripts and proc macros execute on the host. Apply release path
    # remapping only to target code by passing an explicit target triple.
    env = dict(os.environ)
    flags = [f"--remap-path-prefix={home}=/build-home", "--remap-path-prefix=/private/tmp=/build-tmp",
             f"--remap-path-prefix={ROOT}=/src"]
    # CARGO_ENCODED_RUSTFLAGS preserves spaces in checkout paths.
    env.pop("RUSTFLAGS", None)
    env.update(CARGO_ENCODED_RUSTFLAGS="\x1f".join(flags), MACOSX_DEPLOYMENT_TARGET=MIN_MACOS,
               CARGO_PROFILE_RELEASE_STRIP="none", CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_DEBUG="true",
               CC_SHELL_ESCAPED_FLAGS="1", CFLAGS=shlex.join([
                   f"-ffile-prefix-map={ROOT}=/src", f"-ffile-prefix-map={home}=/build-home"]))
    run("cargo", "build", "--locked", *(["--offline"] if offline else []), "--release",
        "--target", TARGET, "-p", "meeting-call-pilot", env=env)
    run("pnpm", "install", "--frozen-lockfile", *(["--offline"] if offline else []))
    run("pnpm", "build", env={**os.environ, "CARGO_NET_OFFLINE": "true"} if offline else None)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"], cwd=ROOT))
    return Path(metadata["target_directory"]) / TARGET / "release/meeting-call-pilot"


def copy_web(dest):
    source = ROOT / "apps/call-pilot/dist"
    (dest / "assets").mkdir(parents=True)
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source)
        if path.is_symlink():
            raise ValueError("Linked frontend asset rejected")
        if relative.as_posix() == "assets" and path.is_dir():
            continue
        if (relative.as_posix() != "index.html" and not re.fullmatch(
                r"assets/[a-zA-Z0-9_-]+-[a-zA-Z0-9_-]{8,}\.(js|css|wasm)", relative.as_posix())):
            raise ValueError("Unexpected frontend build content")
        if not path.is_file() or not 0 < path.stat().st_size <= 8 * 1024 * 1024:
            raise ValueError("Invalid frontend asset")
        shutil.copyfile(path, dest / relative)
    modules = list((dest / "assets").glob("*.wasm"))
    if not (dest / "index.html").is_file() or not any((dest / "assets").glob("*.js")) or not modules:
        raise ValueError("Incomplete Rust/WASM frontend build")
    for module in modules:
        with module.open("rb") as data:
            if data.read(8) != b"\x00asm\x01\x00\x00\x00":
                raise ValueError("Invalid WebAssembly asset")


def assemble(root, server):
    app = root / APP
    contents = app / "Contents"
    resources = contents / "Resources"
    launcher = contents / "MacOS/Interpreter"
    helper = contents / "Helpers/meeting-call-pilot"
    launcher.parent.mkdir(parents=True)
    helper.parent.mkdir()
    resources.mkdir()
    version = json.loads((ROOT / "package.json").read_text())["version"]
    (contents / "Info.plist").write_bytes(plistlib.dumps({
        "CFBundleIdentifier": "org.lateralthinkingmachine.realtime-interpreter",
        "CFBundleExecutable": launcher.name, "CFBundlePackageType": "APPL",
        "CFBundleName": "Enny", "CFBundleDisplayName": "Enny", "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleShortVersionString": version, "CFBundleVersion": version,
        "LSMinimumSystemVersion": MIN_MACOS, "NSHighResolutionCapable": True,
    }))
    copy_web(resources / "web")
    shutil.copyfile(ROOT / "LICENSE", resources / "LICENSE")
    run(sys.executable, "tools/binary_notices.py", resources / "notices")
    write_desktop_notice_index(resources)
    shutil.copyfile(server, helper)
    helper.chmod(0o755)
    run("/usr/bin/strip", "-Sx", helper)
    run("/usr/bin/clang", "-arch", "arm64", "-mmacosx-version-min=" + MIN_MACOS, "-Os",
        "-fobjc-arc", "-Werror", "-Wno-deprecated-declarations", "-framework", "Cocoa",
        "tools/macos/Interpreter.m", "-o", launcher)
    # Identity '-' is ad hoc. No signing identity or keychain is accessed.
    for target in (helper, app):
        run("/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", target)
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)
    for binary in (launcher, helper):
        run("/usr/bin/lipo", "-verify_arch", "arm64", binary)
        deps = subprocess.check_output(["/usr/bin/otool", "-L", binary], text=True).splitlines()[1:]
        if any(not dep.strip().startswith(("/usr/lib/", "/System/Library/")) for dep in deps):
            raise ValueError("Binary links a non-system library")
    private_markers = [os.fsencode(path) for path in {Path.home(), ROOT, root}]
    for file in app.rglob("*"):
        if file.is_file() and any(marker in file.read_bytes() for marker in private_markers):
            raise ValueError(f"Build path embedded in {file.name}")
    return app


def archive(app, destination):
    # Retain executable bits while dropping host ownership, xattrs, paths and
    # timestamps. The macOS Archive Utility and ditto both restore these modes.
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as out:
        for path in sorted(app.rglob("*")):
            if path.is_dir():
                continue
            info = zipfile.ZipInfo(path.relative_to(app.parent).as_posix(), (1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            mode = 0o100755 if path.parent.name in ("MacOS", "Helpers") else 0o100644
            info.external_attr = mode << 16
            out.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="directory for the review ZIP and checksum")
    parser.add_argument("--offline", action="store_true", help="use cached Cargo and pnpm dependencies")
    args = parser.parse_args()
    if (platform.system(), platform.machine()) != ("Darwin", "arm64"):
        parser.error("an Apple Silicon macOS build host is required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    server = build(args.offline)
    with tempfile.TemporaryDirectory(dir=output) as staging:
        root = Path(staging)
        app = assemble(root, server)
        temporary = root / ARCHIVE
        archive(app, temporary)
        digest = hashlib.sha256(temporary.read_bytes()).hexdigest()
        temporary.replace(output / ARCHIVE)
        (output / (ARCHIVE + ".sha256")).write_text(f"{digest}  {ARCHIVE}\n")
    print(f"{ARCHIVE}  sha256 {digest}")


if __name__ == "__main__":
    main()
