"""Build the operator or one-file portable Rust/UI client with the locked toolchain."""
import argparse
import fcntl
import hashlib
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

from binary_notices import notice_files

ROOT = Path(__file__).resolve().parents[1]
CRATE = ROOT / "apps/interpreter-ui"
TARGET = CRATE / "target"
WASM_TARGET = "wasm32-unknown-unknown"
BINDGEN_VERSION = "0.2.130"
REGISTRY_COMPONENTS = CRATE / "src/components"


def run(*args, **kwargs):
    return subprocess.check_output(args, cwd=ROOT, text=True, **kwargs)


def find_bindgen():
    for candidate in (shutil.which("wasm-bindgen"), TARGET / "tools/bin/wasm-bindgen"):
        if candidate and Path(candidate).is_file():
            if run(candidate, "--version").strip() == f"wasm-bindgen {BINDGEN_VERSION}":
                return candidate
    raise SystemExit(f"Install the matching generator with: cargo install --locked wasm-bindgen-cli --version {BINDGEN_VERSION}")


class PlainNotice(HTMLParser):
    """Extract the readable content of the rust-docs copyright inventory."""
    def __init__(self):
        super().__init__()
        self.parts = []
        self.hidden = False

    def handle_starttag(self, tag, attrs):
        if tag in {"style", "script"}:
            self.hidden = True
        if tag in {"p", "li", "h1", "h2", "h3", "pre", "br"}:
            self.parts.append("\n")

    def handle_endtag(self, tag):
        if tag in {"style", "script"}:
            self.hidden = False

    def handle_data(self, data):
        if not self.hidden:
            self.parts.append(data)


def licenses_for(pkg):
    path = Path(pkg["manifest_path"]).parent
    files = notice_files(path)
    if files:
        return [(str(f.relative_to(path)), f.read_text(encoding="utf-8")) for f in files]
    # These published archives omit the project's root license. Preserve the
    # upstream text, or the alternative expressly offered in the Cargo metadata.
    if pkg["repository"] == "https://github.com/leptos-rs/leptos" and pkg["license"] == "MIT":
        license_path = CRATE / "licenses/leptos.LICENSE"
    elif pkg["name"] in {"rstml", "tw_merge"} and pkg["license"] == "MIT":
        license_path = CRATE / "licenses" / f"{pkg['name']}.LICENSE"
    elif pkg["license"] == "MIT OR Apache-2.0":
        license_path = ROOT / "LICENSE"
    elif pkg["name"] in {"convert_case_extras", "erased"} and pkg["license"] == "MIT":
        # No LICENSE in either the published archive or its corresponding Git
        # revision. Retain the authors from the manifest and the full MIT grant.
        body = (REGISTRY_COMPONENTS / "LICENSE").read_text().split("Permission is hereby", 1)[1]
        return [("MIT", "MIT License\n\nPermission is hereby" + body)]
    else:
        raise ValueError(f"Missing redistribution notice for {pkg['name']} {pkg['version']}")
    return [(pkg["license"], license_path.read_text(encoding="utf-8"))]


def write_notices(output, feature):
    metadata = json.loads(run("cargo", "metadata", "--manifest-path", CRATE / "Cargo.toml",
                              "--format-version", "1", "--locked", "--offline",
                              "--no-default-features", "--features", feature,
                              "--filter-platform", WASM_TARGET))
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    packages = {pkg["id"]: pkg for pkg in metadata["packages"]}
    seen = set()
    todo = list(metadata["workspace_members"])
    while todo:
        key = todo.pop()
        if key in seen:
            continue
        seen.add(key)
        for dep in nodes[key]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                todo.append(dep["pkg"])

    # Deduplicate identical grant texts. Every package retains a precise
    # mapping to its notices even when dozens of crates use the same grant.
    notices = {}
    lines = ["Rust/UI components (MIT)", (REGISTRY_COMPONENTS / "LICENSE").read_text()]
    for pkg in sorted((packages[key] for key in seen - set(metadata["workspace_members"])),
                      key=lambda p: (p["name"], p["version"])):
        lines.append(f"{pkg['name']} {pkg['version']} ({pkg['license']})")
        if pkg["authors"]:
            lines.append("Authors from package manifest: " + ", ".join(pkg["authors"]))
        for label, contents in licenses_for(pkg):
            digest = hashlib.sha256(contents.encode()).hexdigest()
            notices.setdefault(digest, contents)
            lines.append(f"  {label}: {digest}")
    for digest, contents in sorted(notices.items()):
        lines.extend(["\n" + digest, contents])
    sysroot = Path(run("rustc", "--print", "sysroot").strip())
    rust_notice = sysroot / "share/doc/rust/COPYRIGHT-library.html"
    if not rust_notice.is_file():
        raise SystemExit("Install the pinned standard-library notices with: rustup component add rust-docs")
    reader = PlainNotice()
    reader.feed(rust_notice.read_text(encoding="utf-8"))
    lines.extend(["Rust standard library", "".join(reader.parts)])
    output.write_text("\n".join(lines), encoding="utf-8")


def build(feature, check, offline):
    if not (Path(run("rustc", "--print", "target-libdir", "--target", WASM_TARGET).strip())).is_dir():
        raise SystemExit(f"Install the web target with: rustup target add {WASM_TARGET}")
    generator = find_bindgen()
    destination = (ROOT / "apps/call-pilot/generated" if feature == "operator"
                   else TARGET / "portable-bindgen")
    TARGET.mkdir(exist_ok=True)
    with (TARGET / "wasm-build.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
        flags = (os.environ["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if os.environ.get("CARGO_ENCODED_RUSTFLAGS")
                 else shlex.split(os.environ.get("RUSTFLAGS", "")))
        # Panic locations can otherwise carry the builder's absolute home path.
        flags.extend([f"--remap-path-prefix={ROOT}=/source", f"--remap-path-prefix={cargo_home}=/cargo"])
        env = {**os.environ, "CARGO_ENCODED_RUSTFLAGS": "\x1f".join(flags)}
        subprocess.run(["cargo", "build", "--manifest-path", CRATE / "Cargo.toml", "--release", "--locked",
                        "--no-default-features", "--features", feature, "--target", WASM_TARGET,
                        "--target-dir", TARGET, *(["--offline"] if offline else [])], cwd=ROOT, env=env, check=True)
        binary = TARGET / WASM_TARGET / "release/interpreter_ui.wasm"
        with tempfile.TemporaryDirectory(prefix="bindgen-", dir=TARGET) as temp:
            stage = Path(temp)
            subprocess.run([generator, "--target", "web" if feature == "operator" else "no-modules",
                            "--remove-name-section", "--out-name", "interpreter_ui", "--out-dir", stage,
                            *(["--no-typescript"] if feature == "portable" else []), binary], check=True)
            write_notices(stage / "NOTICES.txt", feature)
            destination.mkdir(exist_ok=True)
            for path in stage.iterdir():
                if path.is_dir():
                    shutil.copytree(path, destination / path.name, dirs_exist_ok=True)
                else:
                    path.replace(destination / path.name)
        if feature == "portable":
            subprocess.run([sys.executable, ROOT / "tools/build_portable_ui.py", destination,
                            *(["--check"] if check else [])], cwd=ROOT, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("edition", choices=("operator", "portable"))
    parser.add_argument("--check", action="store_true", help="check the single-file download without replacing it")
    parser.add_argument("--offline", action="store_true", help="use installed Cargo dependencies only")
    args = parser.parse_args()
    if args.check and args.edition != "portable":
        parser.error("--check applies to the portable download")
    build(args.edition, args.check, args.offline)
