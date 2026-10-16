#!/usr/bin/env python3
"""Write THIRD-PARTY-LICENSES.md and licenses/ for the rosie_mandlebrot binary.

    python tools/third_party.py

Walks `cargo metadata` from the binary through its normal (linked)
dependencies, lists every crate with its licence, and copies the GPL and
LGPL texts that the statically linked GMP and MPFR require to be shipped.
Run it before a release; the release pipeline does.
"""

import json
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

C_LIBRARIES = [
    ("GMP 6.3.0", "LGPL-3.0-or-later OR GPL-2.0-or-later", "https://gmplib.org/",
     "statically linked through gmp-mpfr-sys, for the high-precision reference orbit"),
    ("MPFR 4.2.2", "LGPL-3.0-or-later", "https://www.mpfr.org/",
     "statically linked through gmp-mpfr-sys"),
    ("SQLite", "Public domain", "https://sqlite.org/",
     "statically linked through rusqlite's bundled build, for the checkpoint database"),
    ("mingw-w64 runtime (Windows builds only)", "ZPL-2.1 and MIT", "https://www.mingw-w64.org/",
     "statically linked C runtime and winpthreads"),
]


def main() -> None:
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT, text=True))
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])
    root = next(i for i in workspace if packages[i]["name"] == "rosie_mandlebrot")

    seen, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])

    crates = sorted((packages[i] for i in seen if i not in workspace), key=lambda p: p["name"])
    lines = [
        "# Third-party licences",
        "",
        "Rosie_Mandlebrot is MIT licensed (see `LICENSE`). The release binaries also contain the",
        "following software, under the licences listed.",
        "",
        "## C libraries linked into the binary",
        "",
        "| library | licence | source | use |",
        "|---|---|---|---|",
    ]
    lines += [f"| {n} | {l} | {u} | {w} |" for n, l, u, w in C_LIBRARIES]
    lines += [
        "",
        "**GMP and MPFR are under the GNU LGPL**, and are linked statically. As the LGPL requires, you",
        "may replace them: the complete source of this program is public at",
        "<https://github.com/QueenRose4444/rosie-mandlebrot>, and `cargo build --release` relinks it",
        "against whatever GMP and MPFR you point `gmp-mpfr-sys` at (see its documentation for using",
        "system libraries). The full GPL and LGPL texts are in `licenses/`.",
        "",
        f"## Rust crates ({len(crates)})",
        "",
        "| crate | version | licence | repository |",
        "|---|---|---|---|",
    ]
    for p in crates:
        lines.append(f"| {p['name']} | {p['version']} | {p.get('license') or 'see repository'} | "
                     f"{p.get('repository') or ''} |")
    (ROOT / "THIRD-PARTY-LICENSES.md").write_text("\n".join(lines) + "\n", encoding="utf-8")

    gmp = next(p for p in meta["packages"] if p["name"] == "gmp-mpfr-sys")
    src = Path(gmp["manifest_path"]).parent
    out = ROOT / "licenses"
    out.mkdir(exist_ok=True)
    shutil.copy(src / "LICENSE-LGPL.md", out / "LGPL-3.0.md")
    shutil.copy(src / "LICENSE-GPL.md", out / "GPL-3.0.md")
    print(f"THIRD-PARTY-LICENSES.md: {len(crates)} crates; licenses/ written")


if __name__ == "__main__":
    main()
