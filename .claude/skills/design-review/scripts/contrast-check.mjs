#!/usr/bin/env node
// WCAG 2.x contrast ratio between two colors. Same output as contrast-check.py.
// Usage: node contrast-check.mjs "#RRGGBB" "#RRGGBB"
const parse = (s) => {
  const c = s.trim().toLowerCase();
  let m;
  if ((m = c.match(/^#?([0-9a-f]{3})$/))) return [...m[1]].map((ch) => parseInt(ch + ch, 16));
  if ((m = c.match(/^#?([0-9a-f]{6})$/))) return [0, 2, 4].map((i) => parseInt(m[1].slice(i, i + 2), 16));
  if ((m = c.match(/^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/))) return [m[1], m[2], m[3]].map(Number);
  throw new Error(`unrecognized color: ${s}`);
};
const lum = ([r, g, b]) => {
  const lin = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
};
const [fg, bg] = process.argv.slice(2, 4);
if (!fg || !bg) { console.log('Usage: node contrast-check.mjs "#fg" "#bg"'); process.exit(2); }
const la = lum(parse(fg)), lb = lum(parse(bg));
const ratio = (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
const v = (t) => (ratio >= t ? 'PASS' : 'FAIL');
console.log(`ratio: ${ratio.toFixed(2)}:1`);
console.log(`normal text (4.5): ${v(4.5)}`);
console.log(`large text / UI (3.0): ${v(3.0)}`);
console.log(`AAA (7.0): ${v(7.0)}`);
process.exit(ratio >= 4.5 ? 0 : 1);
