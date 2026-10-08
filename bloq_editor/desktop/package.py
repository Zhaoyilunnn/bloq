#!/usr/bin/env python3
"""Package icon-bearing macOS/Linux editor releases with Python 3.11+ stdlib."""

import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[2]
ICONS = ROOT / "bloq_editor/assets/icons"
INSTALLER = r'''#!/usr/bin/env python3
"""Install this Linux release into the current user's desktop applications."""
import os
from pathlib import Path
import shutil

bundle = Path(__file__).resolve().parent
data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")).expanduser().resolve()
binary = data / "bloq_editor/bloq_editor"
icon = data / "icons/hicolor/256x256/apps/bloq_editor.png"
launcher = data / "applications/bloq_editor.desktop"
if "=" in str(binary) or any(ord(character) < 32 or ord(character) == 127 for character in str(binary)):
    raise SystemExit("Desktop launcher path cannot contain control characters or '='.")
for destination in (binary, icon, launcher):
    destination.parent.mkdir(parents=True, exist_ok=True)
shutil.copy2(bundle / "bloq_editor", binary)
binary.chmod(0o755)
shutil.copytree(bundle / "licenses", binary.parent / "licenses", dirs_exist_ok=True)
shutil.copy2(bundle / "bloq.png", icon)
# Escape the quoted Exec argument, then Desktop Entry backslash escapes.
command = str(binary).replace("\\", "\\\\")
for character in '"`$':
    command = command.replace(character, "\\" + character)
command = command.replace("\\", "\\\\").replace("%", "%%")
desktop = (bundle / "bloq_editor.desktop").read_text()
launcher.write_text(desktop.replace("Exec=bloq_editor", 'Exec="' + command + '"'))
print("Installed Bloq Editor:", launcher)
'''


def package(binary: Path, target: str, output: Path) -> Path:
    """Build a desktop archive and adjacent SHA256 file from an existing binary."""
    if "-apple-darwin" not in target and "-linux-" not in target:
        raise ValueError("desktop bundles support Linux and macOS; Windows uses the .exe icon")
    binary = binary.resolve(strict=True)
    output.mkdir(parents=True, exist_ok=True)
    name = f"bloq_editor-desktop-{target}"
    archive = output / f"{name}.tar.gz"
    with tempfile.TemporaryDirectory() as temporary:
        bundle = Path(temporary) / name
        bundle.mkdir()
        shutil.copy2(ROOT / "LICENSE", bundle / "LICENSE")
        if "-apple-darwin" in target:
            contents = bundle / "Bloq Editor.app/Contents"
            (contents / "MacOS").mkdir(parents=True)
            (contents / "Resources").mkdir()
            executable = contents / "MacOS/bloq_editor"
            licenses = contents / "Resources/licenses"
            shutil.copy2(ICONS / "bloq.icns", contents / "Resources/bloq.icns")
            with (ROOT / "Cargo.toml").open("rb") as manifest:
                version = tomllib.load(manifest)["workspace"]["package"]["version"].split("-")[0]
            with (contents / "Info.plist").open("wb") as metadata:
                plistlib.dump({
                    "CFBundleName": "Bloq Editor",
                    "CFBundleDisplayName": "Bloq Editor",
                    "CFBundleIdentifier": "io.github.inmzhang.bloq-editor",
                    "CFBundleExecutable": "bloq_editor",
                    "CFBundleIconFile": "bloq.icns",
                    "CFBundlePackageType": "APPL",
                    "CFBundleInfoDictionaryVersion": "6.0",
                    "CFBundleShortVersionString": version,
                    "CFBundleVersion": version,
                    "NSHighResolutionCapable": True,
                }, metadata)
        else:
            executable = bundle / "bloq_editor"
            licenses = bundle / "licenses"
            shutil.copy2(ICONS / "bloq-256.png", bundle / "bloq.png")
            shutil.copy2(Path(__file__).with_name("bloq_editor.desktop"), bundle / "bloq_editor.desktop")
            (bundle / "install.py").write_text(INSTALLER)
            (bundle / "README.txt").write_text(
                "Run python3 install.py to install Bloq Editor for the current user.\n"
                "Then launch Bloq Editor from your desktop applications menu.\n"
                "The binary and icon are copied into XDG_DATA_HOME (default ~/.local/share);\n"
                "the extracted archive can be removed after installation.\n"
                "Linux system graphics/audio libraries are still required.\n"
            )
        licenses.mkdir()
        for source in (
            ROOT / "LICENSE",
            ROOT / "THIRD-PARTY-NOTICES.txt",
            ROOT / "bloq_editor/assets/fonts/NOTICE.md",
            *sorted((ROOT / "bloq_editor/assets/fonts").glob("LICENSE-*")),
        ):
            shutil.copy2(source, licenses / source.name)
        shutil.copy2(binary, executable)
        executable.chmod(0o755)

        def public_metadata(info: tarfile.TarInfo) -> tarfile.TarInfo:
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            return info

        with tarfile.open(archive, "w:gz") as packed:
            packed.add(bundle, arcname=name, filter=public_metadata)
    with archive.open("rb") as packed:
        digest = hashlib.file_digest(packed, "sha256").hexdigest()
    archive.with_suffix(".gz.sha256").write_text(f"{digest}  {archive.name}\n")
    return archive


def check() -> None:
    """Check bundle metadata, executable permissions, checksums and Linux installation."""
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        binary = directory / "binary"
        binary.write_bytes(b"test editor executable\n")
        for target in ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin"):
            archive = package(binary, target, directory)
            checksum = archive.with_suffix(".gz.sha256").read_text().split()[0]
            assert checksum == hashlib.sha256(archive.read_bytes()).hexdigest()
            with tarfile.open(archive) as packed:
                assert all(
                    (member.uid, member.gid, member.uname, member.gname) == (0, 0, "", "")
                    for member in packed.getmembers()
                )
                prefix = archive.name.removesuffix(".tar.gz")
                assert packed.extractfile(f"{prefix}/LICENSE").read() == (ROOT / "LICENSE").read_bytes()
                if "apple" in target:
                    contents = f"{prefix}/Bloq Editor.app/Contents"
                    license_prefix = f"{contents}/Resources/licenses"
                    metadata = plistlib.loads(packed.extractfile(f"{contents}/Info.plist").read())
                    assert packed.extractfile(f"{contents}/Resources/{metadata['CFBundleIconFile']}").read().startswith(b"icns")
                    executable = packed.getmember(f"{contents}/MacOS/{metadata['CFBundleExecutable']}")
                    assert executable.mode & 0o111
                else:
                    license_prefix = f"{prefix}/licenses"
                    assert packed.getmember(f"{prefix}/bloq_editor").mode & 0o111
                    bundle = directory / "extracted"
                    bundle.mkdir()
                    for filename in ("bloq_editor", "bloq.png", "bloq_editor.desktop", "install.py"):
                        (bundle / filename).write_bytes(packed.extractfile(f"{prefix}/{filename}").read())
                    (bundle / "licenses").mkdir()
                    for member in packed.getmembers():
                        if member.isfile() and member.name.startswith(f"{license_prefix}/"):
                            (bundle / "licenses" / Path(member.name).name).write_bytes(packed.extractfile(member).read())
                    data = directory / 'share with spaces %f $HOME \\" `test`'
                    subprocess.run([sys.executable, str(bundle / "install.py")], check=True,
                                   env={**os.environ, "XDG_DATA_HOME": str(data)}, capture_output=True)
                    assert (data / "bloq_editor/bloq_editor").read_bytes() == binary.read_bytes()
                    for source in (bundle / "licenses").iterdir():
                        assert (data / "bloq_editor/licenses" / source.name).read_bytes() == source.read_bytes()
                    assert (data / "icons/hicolor/256x256/apps/bloq_editor.png").read_bytes().startswith(b"\x89PNG")
                    launcher = (data / "applications/bloq_editor.desktop").read_text()
                    assert "Icon=bloq_editor" in launcher
                    encoded = next(line[5:] for line in launcher.splitlines() if line.startswith("Exec="))
                    # Undo Desktop Entry escapes; shlex has no $/backtick expansion to quote against.
                    command = encoded.replace("\\\\", "\\").replace("\\$", "$").replace("\\`", "`")
                    assert [argument.replace("%%", "%") for argument in shlex.split(command)] == [str((data / "bloq_editor/bloq_editor").resolve())]
                    for invalid in ("line\nbreak", "carriage\rreturn", "equal=sign"):
                        rejected = directory / invalid
                        result = subprocess.run([sys.executable, str(bundle / "install.py")],
                                                env={**os.environ, "XDG_DATA_HOME": str(rejected)}, capture_output=True)
                        assert result.returncode != 0 and b"cannot contain" in result.stderr
                        assert not rejected.exists()
                for filename in ("LICENSE", "THIRD-PARTY-NOTICES.txt", "NOTICE.md", "LICENSE-FANTASQUE.md", "LICENSE-ZED-IOSEVKA.md", "LICENSE-NERD-FONTS.txt", "LICENSE-FONT-AWESOME.txt"):
                    assert packed.extractfile(f"{license_prefix}/{filename}").read()
    print("Desktop packaging checks passed")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("build", help="package a built macOS/Linux binary")
    build.add_argument("binary", type=Path)
    build.add_argument("target", help="Rust target triple")
    build.add_argument("--output", type=Path, default=ROOT / "target/desktop")
    commands.add_parser("check", help="run packaging and installation checks without installing locally")
    args = parser.parse_args()
    if args.command == "check":
        check()
    else:
        print(package(args.binary, args.target, args.output))


if __name__ == "__main__":
    main()
