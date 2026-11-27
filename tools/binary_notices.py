"""Collect redistribution notices from the resolved inputs to the native release."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
NOTICE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "patents")
SOURCE_TAG = "https://github.com/LateralThinkingMachine/enny-interpreter/blob/v0.1.0-preview.5"


def write_desktop_notice_index(package):
    """Link the listener's bundled licenses and its matching source provenance."""
    (package / "THIRD_PARTY_NOTICES.md").write_text(f"""# Third-party notices

Enny's first-party code is distributed under the [Apache License 2.0](LICENSE).

The [native and web dependency notices](notices/third-party-components.txt) include the MIT and Apache-2.0 licenses for the patched `libfvad-sys` 1.0.0 bindings. They also include the bundled WebRTC voice activity detector's BSD license, patent grant and author credits. The source records the changes and file hashes. [libfvad-sys provenance]({SOURCE_TAG}/crates/adapters/vendor/libfvad-sys/UPSTREAM.json)

The [compiled interface notices](notices/interpreter-ui.txt) include the MIT license for the Rust/UI Button and Card components adapted for Leptos. The source identifies the upstream revisions. [Component provenance]({SOURCE_TAG}/apps/interpreter-ui/src/components/UPSTREAM.md)

The [Rust standard library notices](notices/rust-standard-library.html) cover the library distributed with this download.
""", encoding="utf-8")


def notice_files(package):
    """Include bundled libraries' legal files as well as the root package's."""
    files = []
    for path in package.rglob("*"):
        name = path.name.lower()
        if not (name.startswith(NOTICE_PREFIXES) or name == "authors"):
            continue
        if path.is_symlink() or not path.is_file():
            continue
        files.append(path)
    return sorted(files)


def rust_packages(target):
    data = json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--locked", "--offline",
        "--filter-platform", target,
    ], cwd=ROOT))
    nodes = {node["id"]: node for node in data["resolve"]["nodes"]}
    records = {pkg["id"]: pkg for pkg in data["packages"]}
    pending = list(data["workspace_members"])
    seen = set()
    while pending:
        ident = pending.pop()
        if ident in seen:
            continue
        seen.add(ident)
        for dep in nodes[ident]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
        pkg = records[ident]
        if ident in data["workspace_members"]:
            continue
        path = Path(pkg["manifest_path"]).parent
        files = notice_files(path)
        # The published fvad crate declares this alternative in its manifest,
        # but omits a license file. Ship the complete Apache-2.0 text.
        if pkg["name"] == "fvad" and pkg["version"] == "0.1.3" and not files:
            files = [ROOT / "LICENSE"]
        yield (pkg["name"], pkg["version"], pkg["license"], pkg["authors"], path, files)


def web_packages():
    modules = ROOT / "apps/call-pilot/node_modules"
    manifest = json.loads((modules.parent / "package.json").read_text())
    vite = (modules / "vite").resolve(strict=True)
    # Vite and Rolldown can emit small client-side helpers into the bundle.
    pending = [(modules / dep).resolve(strict=True) for dep in manifest["dependencies"]]
    pending.extend([vite, (vite.parent / "rolldown").resolve(strict=True)])
    seen = set()
    while pending:
        path = pending.pop()
        if path in seen:
            continue
        seen.add(path)
        pkg = json.loads((path / "package.json").read_text())
        if pkg["name"] not in {"vite", "rolldown"}:
            for dep in pkg.get("dependencies", {}):
                paths = (path / "node_modules" / dep, path.parent / dep, modules / dep)
                pending.append(next(p for p in paths if p.exists()).resolve(strict=True))
        yield (pkg["name"], pkg["version"], pkg["license"], [], path, notice_files(path))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--target", default="aarch64-apple-darwin",
                        choices=("aarch64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"))
    args = parser.parse_args()
    output = args.output
    output.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / "apps/call-pilot/generated/NOTICES.txt", output / "interpreter-ui.txt")
    records = sorted([*rust_packages(args.target), *web_packages()], key=lambda pkg: pkg[:2])
    with (output / "third-party-components.txt").open("w", encoding="utf-8") as stream:
        for name, version, license_id, authors, path, files in records:
            if not files:
                raise RuntimeError(f"Missing redistribution notice for {name} {version}")
            stream.write(f"\n{'=' * 72}\n{name} {version} ({license_id})\n")
            if authors:
                stream.write(f"Authors from package manifest: {', '.join(authors)}\n")
            for file in files:
                label = file.relative_to(path) if file.is_relative_to(path) else file.name
                stream.write(f"\n--- {label} ---\n")
                stream.write(file.read_text(encoding="utf-8") + "\n")
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    shutil.copyfile(sysroot / "share/doc/rust/COPYRIGHT-library.html",
                    output / "rust-standard-library.html")


if __name__ == "__main__":
    main()
