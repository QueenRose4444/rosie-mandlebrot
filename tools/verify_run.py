#!/usr/bin/env python3
"""Check that a completed run actually rendered something.

    python tools/verify_run.py RUN_DIR

A run can look healthy by every signal it emits itself (no warnings, a clean
stop reason, a video encoded) and still have produced byte-identical pure-white
frames. So this checks the things the run cannot check about itself:

  1. Are the frames actually different from each other?
  2. Do they contain more than one colour?
  3. Did the iteration budget ever have room, or was every frame pinned at its
     ceiling with nothing escaping? (frame_metric.saturated_frac == 1.0 and
     escape_pct == 0 is the signature of iteration starvation.)

Exit code is non-zero when the run looks blank, so it can gate a CI step.
"""

from pathlib import Path
import sqlite3
import sys


def die(msg: str) -> int:
    print(f"  FAIL  {msg}")
    return 1


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    run = Path(sys.argv[1])
    out = run / "out"
    db = run / "state.db"
    bad = 0

    # ---- 1. frames differ from one another ---------------------------------
    pngs = sorted(out.glob("frame_*.png"))
    if not pngs:
        return die(f"no frames in {out}")

    sizes = {}
    for p in pngs:
        sizes.setdefault(p.stat().st_size, []).append(p.name)
    biggest = max(sizes.items(), key=lambda kv: len(kv[1]))
    share = len(biggest[1]) / len(pngs)
    print(f"  {len(pngs)} frames, {len(sizes)} distinct file sizes")
    if share > 0.5:
        bad |= die(
            f"{share * 100:.0f}% of frames share one byte size ({biggest[0]:,} B) "
            "- a flat image compresses to a constant size, so this is the "
            "signature of a blank run"
        )

    # ---- 2. the frames contain more than one colour ------------------------
    try:
        from PIL import Image
    except ImportError:
        print("  SKIP  colour check (needs Pillow)")
    else:
        # First, middle, last - blankness usually arrives and then stays.
        for p in (pngs[0], pngs[len(pngs) // 2], pngs[-1]):
            colours = len(Image.open(p).convert("RGB").getcolors(maxcolors=1 << 20) or [])
            print(f"  {p.name}: {colours} distinct colours")
            if colours <= 2:
                bad |= die(f"{p.name} has {colours} colours - that is a blank frame")

    # ---- 3. the budget was not pinned with nothing escaping ----------------
    if not db.is_file():
        print(f"  SKIP  budget check (no {db})")
        return bad

    c = sqlite3.connect(db)
    try:
        rows = c.execute(
            "SELECT COUNT(*), SUM(saturated_frac >= 0.999 AND escape_pct = 0) FROM frame_metric"
        ).fetchone()
    except sqlite3.OperationalError:
        print("  SKIP  budget check (no frame_metric table)")
        return bad

    total, starved = rows[0], rows[1] or 0
    if total:
        pct = 100.0 * starved / total
        print(f"  {starved}/{total} frames fully saturated with zero escapes ({pct:.0f}%)")
        if pct > 25.0:
            bad |= die(
                "most frames hit their iteration ceiling with nothing escaping - "
                "that is iteration starvation, not a dead end. The frames "
                "are white because the budget ran out, not because there is "
                "nothing there."
            )

    if not bad:
        print("  OK    the run rendered real, varying frames")
    return bad


if __name__ == "__main__":
    raise SystemExit(main())
