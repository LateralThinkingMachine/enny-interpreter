"""Build and exercise a Linux desktop download in isolated local Docker containers."""
import argparse
import gzip
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

from binary_notices import write_desktop_notice_index
from build_macos_release import copy_web


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"arm64": ("aarch64-unknown-linux-gnu", 183), "amd64": ("x86_64-unknown-linux-gnu", 62)}
PYTHON_IMAGE = "python:3.11-slim-bookworm@sha256:ef56ae98435cc40d8a27020b0b32a9941515acd5840f0fba34858bb34dd362d1"


def run(*args, **kwargs):
    subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def stage_inputs(context, offline):
    # Only source needed to compile the reviewed native service enters Docker.
    vendor = "crates/adapters/vendor/libfvad-sys"
    tracked = subprocess.check_output(
        ["git", "ls-files", "-z", "--", vendor, ":(glob)services/call-pilot/src/**/*.rs"], cwd=ROOT)
    inputs = [ROOT / os.fsdecode(name) for name in tracked.split(b"\0") if name]
    inputs.extend(ROOT / name for name in ("Cargo.toml", "Cargo.lock", "services/call-pilot/Cargo.toml"))
    for path in inputs:
        if not path.is_file() or path.is_symlink():
            raise ValueError("Expected regular Rust build input")
        destination = context / path.relative_to(ROOT)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    shutil.copyfile(ROOT / "tools/build_linux_release.Dockerfile", context / "Dockerfile")
    # Never mount the maintainer's Cargo home or forward registry credentials.
    command = ["cargo", "vendor", "--locked", *(["--offline"] if offline else []), str(context / "vendor")]
    subprocess.run(command, cwd=ROOT, stdout=subprocess.DEVNULL, check=True)
    (context / ".cargo").mkdir()
    (context / ".cargo/config.toml").write_text(
        '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
        '[source.vendored-sources]\ndirectory = "/src/vendor"\n')


def inspect_binary(build, arch):
    binary = build / "meeting-call-pilot"
    data = binary.read_bytes()
    # Verify 64-bit little-endian ELF, architecture and dynamic runtime surface.
    if data[:6] != b"\x7fELF\x02\x01" or int.from_bytes(data[18:20], "little") != TARGETS[arch][1]:
        raise ValueError("Unexpected Linux ABI")
    dependencies = (build / "libraries.txt").read_text()
    for line in dependencies.splitlines():
        lib = Path(line.strip().split()[0]).name
        if lib not in {"linux-vdso.so.1", "libc.so.6", "libgcc_s.so.1", "libm.so.6",
                       "ld-linux-aarch64.so.1", "ld-linux-x86-64.so.2"} or "not found" in line:
            raise ValueError("Unreviewed or missing Linux runtime library: " + lib)
    versions = re.findall(r"\bGLIBC_(\d+)\.(\d+)", (build / "symbols.txt").read_text())
    latest = max((int(major), int(minor)) for major, minor in versions)
    if latest > (2, 36):
        raise ValueError("Linux binary exceeds the documented glibc 2.36 baseline")
    return binary


def assemble(package, build, arch):
    binary = inspect_binary(build, arch)
    (package / "bin").mkdir(parents=True)
    shutil.copyfile(binary, package / "bin/meeting-call-pilot")
    (package / "bin/meeting-call-pilot").chmod(0o755)
    copy_web(package / "web")
    (package / "tools").mkdir()
    for name in ("local_call_pilot.py", "open-operator.html"):
        shutil.copyfile(ROOT / "tools" / name, package / "tools" / name)
    shutil.copyfile(ROOT / "tools/linux/start.py", package / "start.py")
    (package / "start.py").chmod(0o755)
    shutil.copyfile(ROOT / "LICENSE", package / "LICENSE")
    run(sys.executable, "tools/binary_notices.py", package / "notices", "--target", TARGETS[arch][0])
    write_desktop_notice_index(package)
    (package / "README.txt").write_text(
        "Enny — Real-time Interpreter for Linux\n\n"
        "Requires a Linux desktop with glibc 2.36 or newer, Python 3.10+, CA certificates, "
        "and Google Chrome for meeting-tab audio. Extract the complete archive in your account.\n\n"
        "Open a terminal in the extracted directory. Start free rehearsal with python3 start.py. "
        "The ten-minute local rehearsal costs US$0.00 and needs no key. For live spoken "
        "interpretation billed by your own OpenAI account, run python3 start.py --live. Choose a meeting "
        "length from 1 to 60 minutes. Choose your local cost limit in US dollars. "
        "Press Enter at each prompt to use 60 minutes and US$5.00. Review the four session "
        "checks. Review the estimated OpenAI cost and your local limit. Confirm only if you agree. "
        "Enter your OpenAI key at the hidden prompt. Opening the operator page is free. "
        "OpenAI charges can begin when live provider connections open.\n\n"
        "Open the local operator page in Chrome, select the private credentials file printed by "
        "the launcher and then choose the meeting tab with audio. Keep the operator tab private. "
        "The two listeners use 127.0.0.1 ports 5180 and 5182. Keep this terminal open. "
        "Press Ctrl-C to close the local service.\n\n"
        "Live processing sends selected audio to OpenAI. The full-hour local estimate for captions "
        "and interpretation is US$4.02 per computer. Your provider controls billing.\n")
    for file in package.rglob("*"):
        if file.is_file():
            data = file.read_bytes()
            if any(os.fsencode(path) in data for path in (ROOT, Path.home(), build, package)):
                raise ValueError("Build account or private path embedded in " + file.name)


def archive(package, path):
    with path.open("wb") as output, gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0) as stream:
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            for file in sorted(package.rglob("*")):
                if file.is_dir():
                    continue
                relative = file.relative_to(package)
                info = tarfile.TarInfo(f"{package.name}/{relative.as_posix()}")
                info.size = file.stat().st_size
                info.mode = 0o755 if relative.as_posix() in ("bin/meeting-call-pilot", "start.py") else 0o644
                with file.open("rb") as src:
                    tar.addfile(info, src)


def smoke(path, arch, name):
    # Pass the exact release bytes on stdin. The non-root test process has a
    # disposable writable /tmp, a read-only OS, loopback, and zero external network.
    script = ('cd /tmp && tar xf - --no-same-owner && tar xzf download.tar.gz --no-same-owner && '
              'cd "$1" && python3 -I start.py --check && python3 -I start.py --check-live-config && '
              'python3 -I /tmp/interactive.py .')
    with tempfile.TemporaryFile() as packed:
        with tarfile.open(fileobj=packed, mode="w") as envelope:
            for name_in_test, file in (("download.tar.gz", path), ("interactive.py", ROOT / "tools/test_linux_download.py")):
                info = tarfile.TarInfo(name_in_test)
                info.size = file.stat().st_size
                with file.open("rb") as source:
                    envelope.addfile(info, source)
        packed.seek(0)
        run("docker", "run", "--rm", "-i", "--platform", "linux/" + arch, "--network", "none",
            "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
            "--user", "65534:65534", "--tmpfs", "/tmp:rw,exec,nosuid,nodev,mode=1777",
            PYTHON_IMAGE, "sh", "-ec", script, "check", name, stdin=packed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--arch", required=True, choices=TARGETS)
    parser.add_argument("--offline", action="store_true",
                        help="use locally cached Cargo and pnpm dependencies; Docker still needs its images and compiler layer")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    run("pnpm", "install", "--frozen-lockfile", *(["--offline"] if args.offline else []))
    run("pnpm", "build", env={**os.environ, "CARGO_NET_OFFLINE": "true"} if args.offline else None)
    with tempfile.TemporaryDirectory() as temp:
        scratch = Path(temp).resolve()
        context = scratch / "inputs"
        context.mkdir()
        stage_inputs(context, args.offline)
        build = scratch / "compiled"
        run("docker", "buildx", "build", "--platform", "linux/" + args.arch,
            "--output", f"type=local,dest={build}", str(context))
        package = scratch / ("real-time-interpreter-linux-" + args.arch)
        assemble(package, build, args.arch)
        with tempfile.TemporaryDirectory(dir=output) as ready:
            candidate = Path(ready) / (package.name + ".tar.gz")
            archive(package, candidate)
            smoke(candidate, args.arch, package.name)
            digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
            destination = output / candidate.name
            candidate.replace(destination)
            destination.with_suffix(".gz.sha256").write_text(f"{digest}  {candidate.name}\n")
            print(f"{candidate.name}  sha256 {digest}")


if __name__ == "__main__":
    main()
