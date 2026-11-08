// Synthesizes public/sounds/cat-meow.wav (Nectar's own, no samples): a harmonic tone whose pitch rises and
// falls while vowel formants move "m" → "ee-a" → "ow". Re-run: node scripts/make-meow.mjs
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const RATE = 22050;
const DUR = 0.62;
const n = Math.round(RATE * DUR);

const lerp = (a, b, t) => a + (b - a) * t;
const ease = (t) => t * t * (3 - 2 * t);
// Piecewise curve through [time 0..1, value] points.
const curve = (pts) => (t) => {
  for (let i = 1; i < pts.length; i++) {
    if (t <= pts[i][0]) return lerp(pts[i - 1][1], pts[i][1], ease((t - pts[i - 1][0]) / (pts[i][0] - pts[i - 1][0])));
  }
  return pts[pts.length - 1][1];
};

const pitch = curve([[0, 520], [0.3, 780], [0.55, 720], [1, 430]]);
const f1 = curve([[0, 320], [0.25, 850], [0.6, 700], [1, 420]]);
const f2 = curve([[0, 1900], [0.25, 1750], [0.6, 1200], [1, 850]]);
const f3 = curve([[0, 2900], [1, 2500]]);
const gain = curve([[0, 0], [0.07, 1], [0.7, 0.85], [1, 0]]);

const peak = (f, centre, width) => 1 / (1 + ((f - centre) / width) ** 2);

const out = new Float32Array(n);
const phases = new Float64Array(16);
for (let i = 0; i < n; i++) {
  const t = i / n;
  const f0 = pitch(t) * (1 + 0.012 * Math.sin(2 * Math.PI * 5.5 * (i / RATE)));
  let v = 0;
  for (let k = 1; k <= phases.length; k++) {
    const f = f0 * k;
    if (f > RATE / 2 - 500) break;
    phases[k - 1] += (2 * Math.PI * f) / RATE;
    const amp = (1 / k) * (peak(f, f1(t), 140) + 0.7 * peak(f, f2(t), 200) + 0.35 * peak(f, f3(t), 300) + 0.04);
    v += amp * Math.sin(phases[k - 1]);
  }
  out[i] = v * gain(t);
}

let max = 0;
for (const v of out) max = Math.max(max, Math.abs(v));
const pcm = Buffer.alloc(44 + n * 2);
pcm.write("RIFF", 0);
pcm.writeUInt32LE(36 + n * 2, 4);
pcm.write("WAVEfmt ", 8);
pcm.writeUInt32LE(16, 16);
pcm.writeUInt16LE(1, 20);
pcm.writeUInt16LE(1, 22);
pcm.writeUInt32LE(RATE, 24);
pcm.writeUInt32LE(RATE * 2, 28);
pcm.writeUInt16LE(2, 32);
pcm.writeUInt16LE(16, 34);
pcm.write("data", 36);
pcm.writeUInt32LE(n * 2, 40);
for (let i = 0; i < n; i++) pcm.writeInt16LE(Math.round((out[i] / max) * 0.7 * 32767), 44 + i * 2);

writeFileSync(join(root, "public/sounds/cat-meow.wav"), pcm);
console.log(`ok ${n} samples, ${pcm.length} bytes`);
