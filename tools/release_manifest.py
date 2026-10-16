#!/usr/bin/env python3
"""Write SHA256SUMS and latest.json for the archives in dist/.

    python tools/release_manifest.py v0.1.0

latest.json is what an updater reads from
https://github.com/QueenRose4444/rosie-mandlebrot/releases/latest/download/latest.json:
the newest version and, per platform, its download URL and SHA-256. The hashes are
trustworthy only through SHA256SUMS.minisig, signed with the key in minisign.pub.
"""

import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "QueenRose4444/rosie-mandlebrot"


def main() -> None:
    tag = sys.argv[1] if len(sys.argv) > 1 else ""
    if not tag.startswith("v"):
        raise SystemExit(__doc__)
    dist = ROOT / "dist"
    archives = sorted(p for p in dist.iterdir() if p.name.endswith((".zip", ".tar.xz")))
    if not archives:
        raise SystemExit("no archives in dist/")
    sums, platforms = [], {}
    for a in archives:
        digest = hashlib.sha256(a.read_bytes()).hexdigest()
        sums.append(f"{digest}  {a.name}")
        for plat in ("windows-x86_64", "linux-x86_64"):
            if plat in a.name:
                platforms[plat] = {
                    "url": f"https://github.com/{REPO}/releases/download/{tag}/{a.name}",
                    "sha256": digest,
                    "size": a.stat().st_size,
                }
    (dist / "SHA256SUMS").write_text("\n".join(sums) + "\n", encoding="utf-8")
    manifest = {
        "version": tag[1:],
        "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "notes": f"https://github.com/{REPO}/releases/tag/{tag}",
        "signature": "SHA256SUMS.minisig",
        "platforms": platforms,
    }
    (dist / "latest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print("\n".join(sums))


if __name__ == "__main__":
    main()
