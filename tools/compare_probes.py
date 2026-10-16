#!/usr/bin/env python3
"""Compare two probe PNGs pixel by pixel.

Used to gate arithmetic changes in the deep shaders: render the same coordinate
with a high-precision engine as reference, then with the engine being changed
before and after, and check the change did not move the result further from the
reference than it already was.

    python tools/compare_probes.py reference.png candidate.png

Reports the share of differing pixels and the size of the disagreement. Both
matter and they answer different questions: a large number of 1/255 differences
is rounding and is fine, while a handful of large ones is a real divergence in
a few pixels — which is exactly what a precision regression looks like.
"""

from pathlib import Path
import sys

try:
    from PIL import Image
except ImportError:
    print("needs Pillow:  pip install pillow", file=sys.stderr)
    raise SystemExit(2)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2

    ap, bp = Path(sys.argv[1]), Path(sys.argv[2])
    a = Image.open(ap).convert("RGB")
    b = Image.open(bp).convert("RGB")
    if a.size != b.size:
        print(f"size mismatch: {a.size} vs {b.size}", file=sys.stderr)
        return 1

    pa, pb = a.load(), b.load()
    w, h = a.size
    total = w * h

    differing = 0
    sum_delta = 0
    max_delta = 0
    # How the disagreement is distributed. A precision change should show up as
    # a mass of tiny deltas; a correctness break shows up in the tail.
    buckets = {1: 0, 2: 0, 4: 0, 8: 0, 16: 0, 32: 0, 256: 0}

    for y in range(h):
        for x in range(w):
            ca, cb = pa[x, y], pb[x, y]
            d = max(abs(ca[0] - cb[0]), abs(ca[1] - cb[1]), abs(ca[2] - cb[2]))
            if d:
                differing += 1
                sum_delta += d
                if d > max_delta:
                    max_delta = d
                for edge in sorted(buckets):
                    if d <= edge:
                        buckets[edge] += 1
                        break

    print(f"  {ap.parent.name}  vs  {bp.parent.name}")
    if not differing:
        print(f"  identical across all {total:,} pixels")
        return 0

    pct = 100.0 * differing / total
    print(f"  differing pixels : {differing:,} / {total:,}  ({pct:.3f}%)")
    print(f"  max channel delta: {max_delta} / 255")
    print(f"  mean delta (over differing pixels): {sum_delta / differing:.2f}")
    print("  distribution:")
    for edge in sorted(buckets):
        if buckets[edge]:
            label = f"<={edge}" if edge < 256 else ">32"
            print(f"    delta {label:>4} : {buckets[edge]:,}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
