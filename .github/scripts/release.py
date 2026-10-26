#!/usr/bin/env python3
"""Validate release tags, package native binaries, and generate SHA-256 sums."""

import argparse
from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import re
import sys
import tarfile
import tempfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parents[2]
TARGETS = (
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)
RESOURCES = (
    "README.md",
    "examples/filemux.toml",
    "assets/fonts/OFL.txt",
    "docs/phase-1.md",
    "docs/progress.md",
    "docs/previews/phase-1.webp",
)
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)
ARCHIVE_SUFFIXES = (
    ".zip", ".tar", ".gz", ".bz2", ".xz", ".tgz", ".tbz", ".tbz2",
    ".txz", ".7z", ".rar", ".zst",
)


def release_version(tag):
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        cargo = tomllib.load(manifest)
    version = cargo.get("workspace", {}).get("package", {}).get("version")
    match = SEMVER.fullmatch(version) if isinstance(version, str) else None
    if match is None or any(
        part.isdecimal() and len(part) > 1 and part.startswith("0")
        for part in (match.group(4) or "").split(".")
    ):
        raise ValueError("workspace.package.version must be a safe, valid SemVer string")
    expected = f"v{version}"
    if tag != expected:
        raise ValueError(f"release tag must exactly match {expected!r}; received {tag!r}")
    return version


def archive_name(tag, target):
    suffix = ".zip" if target == "x86_64-pc-windows-msvc" else ".tar.gz"
    return f"filemux-{tag}-{target}{suffix}"


@contextmanager
def new_output(destination):
    """Publish a completed file atomically, without replacing existing output."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise FileExistsError(f"output already exists: {destination}")
    descriptor, name = tempfile.mkstemp(
        prefix=".filemux-release-", suffix=".tmp", dir=destination.parent
    )
    os.close(descriptor)
    temporary = Path(name)
    try:
        yield temporary
        # Unlike rename/replace, link refuses an existing destination on every OS.
        os.link(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def metadata(args):
    version = release_version(args.tag)
    print(f"tag={args.tag}")
    print(f"version={version}")
    print(f"prerelease={str('-' in version.split('+', 1)[0]).lower()}")


def package(args):
    release_version(args.tag)
    windows = args.target == "x86_64-pc-windows-msvc"
    extension = ".exe" if windows else ""
    binaries = tuple(f"{name}{extension}" for name in ("filemux", "filemux-gui"))
    sources = [(args.binary_dir / name, name) for name in binaries]
    sources.extend((ROOT / name, name) for name in RESOURCES)
    for source, _ in sources:
        if not source.is_file():
            raise ValueError(f"required release file is missing or not a regular file: {source}")

    name = archive_name(args.tag, args.target)
    directory = f"filemux-{args.tag}-{args.target}"
    destination = args.output_dir / name
    with new_output(destination) as temporary:
        if windows:
            with zipfile.ZipFile(
                temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6
            ) as archive:
                for source, relative in sources:
                    archive.write(source, arcname=f"{directory}/{relative}")
        else:
            with tarfile.open(temporary, "w:gz", dereference=True) as archive:
                for source, relative in sources:
                    info = archive.gettarinfo(source, arcname=f"{directory}/{relative}")
                    if relative in binaries:
                        info.mode |= 0o111
                    with source.open("rb") as contents:
                        archive.addfile(info, contents)
    print(destination)


def checksums(args):
    release_version(args.tag)
    expected = {archive_name(args.tag, target) for target in TARGETS}
    found = {
        entry.name
        for entry in args.artifacts_dir.iterdir()
        if entry.name.lower().endswith(ARCHIVE_SUFFIXES)
    }
    missing = sorted(expected - found)
    extra = sorted(found - expected)
    if missing or extra:
        details = []
        if missing:
            details.append(f"missing archives: {', '.join(missing)}")
        if extra:
            details.append(f"unexpected archives: {', '.join(extra)}")
        raise ValueError("; ".join(details))
    for name in sorted(expected):
        if not (args.artifacts_dir / name).is_file():
            raise ValueError(f"release archive is not a regular file: {name}")

    destination = args.artifacts_dir / "SHA256SUMS"
    with new_output(destination) as temporary:
        with temporary.open("w", encoding="ascii", newline="\n") as sums:
            for name in sorted(expected):
                digest = hashlib.sha256()
                with (args.artifacts_dir / name).open("rb") as archive:
                    while chunk := archive.read(1024 * 1024):
                        digest.update(chunk)
                sums.write(f"{digest.hexdigest()}  {name}\n")
    print(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    metadata_parser = commands.add_parser("metadata", help="validate the workspace release tag")
    metadata_parser.add_argument("--tag", required=True)
    metadata_parser.set_defaults(run=metadata)

    package_parser = commands.add_parser("package", help="package native CLI and GUI binaries")
    package_parser.add_argument("--tag", required=True)
    package_parser.add_argument("--target", required=True, choices=TARGETS)
    package_parser.add_argument("--binary-dir", required=True, type=Path)
    package_parser.add_argument("--output-dir", required=True, type=Path)
    package_parser.set_defaults(run=package)

    sums_parser = commands.add_parser("checksums", help="hash the complete four-target release")
    sums_parser.add_argument("--tag", required=True)
    sums_parser.add_argument("--artifacts-dir", required=True, type=Path)
    sums_parser.set_defaults(run=checksums)
    args = parser.parse_args()
    try:
        args.run(args)
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"release: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
