#!/usr/bin/env python3
"""Package a release build into the archive users download.

    python tools/package.py windows   # -> dist/rosie_mandlebrot-<version>-windows-x86_64.zip
    python tools/package.py linux     # -> dist/rosie_mandlebrot-<version>-linux-x86_64.tar.xz

Expects `tools/release-build.sh` and `tools/third_party.py` to have run. Each archive unpacks to
one folder holding the binary, a portable.txt (so its runs stay inside that folder), the README,
the licence and the third-party licences.
"""

import re
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PORTABLE = (
    "This file makes Rosie_Mandlebrot keep its runs, checkpoints and logs in this folder.\n"
    "Delete it to use your user data folder instead.\n"
)


def version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    return re.search(r'^version\s*=\s*"([^"]+)"', text, re.M).group(1)


def main() -> None:
    platform = sys.argv[1] if len(sys.argv) > 1 else ""
    if platform not in ("windows", "linux"):
        raise SystemExit(__doc__)
    exe = "rosie_mandlebrot.exe" if platform == "windows" else "rosie_mandlebrot"
    binary = ROOT / "target" / "release" / exe
    if not binary.is_file():
        raise SystemExit(f"missing {binary}; run tools/release-build.sh first")
    name = f"rosie_mandlebrot-{version()}-{platform}-x86_64"
    files = [(binary, exe)]
    for f in ["README.md", "LICENSE", "THIRD-PARTY-LICENSES.md", "minisign.pub"]:
        files.append((ROOT / f, f))
    missing = [str(p) for p, _ in files if not p.is_file()]
    if missing:
        raise SystemExit(f"missing: {missing}")

    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    if platform == "windows":
        out = dist / f"{name}.zip"
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for src, arc in files:
                z.write(src, f"{name}/{arc}")
            z.writestr(f"{name}/portable.txt", PORTABLE)
    else:
        out = dist / f"{name}.tar.xz"
        portable = dist / "portable.txt"
        portable.write_text(PORTABLE, encoding="utf-8")
        with tarfile.open(out, "w:xz") as t:
            for src, arc in files + [(portable, "portable.txt")]:
                info = t.gettarinfo(src, f"{name}/{arc}")
                info.mode = 0o755 if arc == exe else 0o644
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                with open(src, "rb") as fh:
                    t.addfile(info, fh)
        portable.unlink()
    print(out)


if __name__ == "__main__":
    main()
