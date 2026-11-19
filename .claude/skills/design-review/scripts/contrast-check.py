#!/usr/bin/env python3
"""WCAG 2.x contrast ratio between two colors.

Usage: contrast-check.py "#RRGGBB" "#RRGGBB" [--large]
Prints the ratio and PASS/FAIL for normal text (4.5:1), large text and UI (3:1), AAA (7:1).
Exit code 0 when normal text passes, 1 otherwise. Accepts #RGB, #RRGGBB, rgb(r,g,b).
"""
import re
import sys


def parse(color: str):
    c = color.strip().lower()
    m = re.fullmatch(r"#?([0-9a-f]{3})", c)
    if m:
        return tuple(int(ch * 2, 16) for ch in m.group(1))
    m = re.fullmatch(r"#?([0-9a-f]{6})", c)
    if m:
        h = m.group(1)
        return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))
    m = re.fullmatch(r"rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+).*\)", c)
    if m:
        return tuple(int(m.group(i)) for i in (1, 2, 3))
    raise ValueError(f"unrecognized color: {color}")


def luminance(rgb):
    def lin(v):
        v = v / 255
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (lin(v) for v in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def ratio(a, b):
    la, lb = luminance(a), luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


def main(argv):
    if len(argv) < 3:
        print(__doc__)
        return 2
    fg, bg = parse(argv[1]), parse(argv[2])
    r = ratio(fg, bg)
    verdict = lambda t: "PASS" if r >= t else "FAIL"
    print(f"ratio: {r:.2f}:1")
    print(f"normal text (4.5): {verdict(4.5)}")
    print(f"large text / UI (3.0): {verdict(3.0)}")
    print(f"AAA (7.0): {verdict(7.0)}")
    return 0 if r >= 4.5 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
