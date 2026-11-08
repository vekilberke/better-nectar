import { describe, expect, test } from "bun:test";
import {
  behindMask, canRest, CAT_EDGE, CAT_GAP, CAT_IDLE_EXTRA, decide, HYBRID_CAT_EXTRA, mergeSpans, MIN_WALK, nearestRest,
  pawFrame, pickWalk, QUIET, restSpans, sense, stretchesOnReturn, wantedPose, type Senses, type Span,
} from "./catBrain";
import { parseNotchIdle } from "./catPalette";
import { CAT_W } from "./components/sonderCatData";

const typing: Senses = { ...QUIET, typing: true };

describe("cat reactions", () => {
  test("typing beats start typing once, the stop ends it", () => {
    let s = sense(QUIET, { kind: "typing", beat: 1 });
    expect(s.typing).toBe(true);
    const same = sense(s, { kind: "typing", beat: 2 });
    expect(same).toBe(s);
    s = sense(s, { kind: "typing-stop" });
    expect(s).toEqual(QUIET);
  });

  test("typing wins over scrolling, scrolling over hiding", () => {
    expect(wantedPose({ typing: true, scrolling: true, near: true })).toBe("type");
    expect(wantedPose({ typing: false, scrolling: true, near: true })).toBe("play");
    expect(wantedPose({ ...QUIET, near: true })).toBe("peek");
    expect(wantedPose(QUIET)).toBeNull();
  });

  test("an idle or walking cat drops what it does to type along", () => {
    expect(decide("sit", typing)).toBe("type");
    expect(decide("run", typing)).toBe("type");
    expect(decide("groom", typing)).toBe("type");
    expect(decide("type", typing)).toBeNull();
  });

  test("stretch and yawn finish first", () => {
    expect(decide("stretch", typing)).toBeNull();
    expect(decide("yawn", typing)).toBeNull();
  });

  test("a sleeping cat wakes to type but not for scrolling or the cursor", () => {
    expect(decide("sleep", typing)).toBe("type");
    expect(decide("sleep", { ...QUIET, scrolling: true })).toBeNull();
    expect(decide("sleep", { ...QUIET, near: true })).toBeNull();
  });

  test("when the user stops, a reacting cat goes back to wandering", () => {
    expect(decide("type", QUIET)).toBe("resume");
    expect(decide("play", QUIET)).toBe("resume");
    expect(decide("peek", QUIET)).toBe("resume");
    expect(decide("sit", QUIET)).toBeNull();
    expect(decide("type", { ...QUIET, scrolling: true })).toBe("play");
  });

  test("coming back stretches unless already typing", () => {
    expect(stretchesOnReturn("sleep")).toBe(true);
    expect(stretchesOnReturn("sit")).toBe(true);
    expect(stretchesOnReturn("type")).toBe(false);
  });

  test("paws alternate per beat", () => {
    expect(pawFrame(1)).not.toBe(pawFrame(2));
    expect(pawFrame(3)).toBe(pawFrame(1));
  });

  test("idle mode defaults to hybrid", () => {
    expect(parseNotchIdle(null)).toBe("hybrid");
    expect(parseNotchIdle("hybrid")).toBe("hybrid");
    expect(parseNotchIdle("cat")).toBe("cat");
    expect(parseNotchIdle("clock")).toBe("clock");
    expect(parseNotchIdle("bogus")).toBe("hybrid");
  });
});

// Collapsed notch: 140 px with the clock, 12 px padding each side. HH:mm is about 38 px wide, centred.
const PLAIN = 140;
const halfOf = (extra: number) => (PLAIN + extra - 24) / 2;
const CLOCK: Span = [-20, 20];
const hybridRest = () => restSpans(halfOf(HYBRID_CAT_EXTRA), [CLOCK], CAT_W, CAT_GAP, CAT_EDGE);
const catRest = () => restSpans(halfOf(CAT_IDLE_EXTRA), [], CAT_W, CAT_GAP, CAT_EDGE);
const spots = (spans: Span[]) => spans.flatMap(([s, e]) => Array.from({ length: e - s + 1 }, (_, i) => s + i));
const overlaps = (x: number, [a, b]: Span) => x < b && x + CAT_W > a;
const RNDS = [0, 0.13, 0.37, 0.5, 0.71, 0.9999];

describe("cat roaming", () => {
  test("the wider notch stays within 90 px of the plain clock notch", () => {
    expect(HYBRID_CAT_EXTRA).toBeLessThanOrEqual(90);
    expect(CAT_IDLE_EXTRA).toBeLessThanOrEqual(90);
  });

  test("hybrid: one stopping area on each side of the clock, clear of it and inside the notch", () => {
    const rest = hybridRest();
    expect(rest).toEqual([[-94, -50], [24, 68]]);
    for (const x of spots(rest)) {
      expect(overlaps(x, [CLOCK[0] - CAT_GAP, CLOCK[1] + CAT_GAP])).toBe(false);
      expect(x).toBeGreaterThanOrEqual(-halfOf(HYBRID_CAT_EXTRA) + CAT_EDGE);
      expect(x + CAT_W).toBeLessThanOrEqual(halfOf(HYBRID_CAT_EXTRA) - CAT_EDGE);
    }
  });

  test("walks are at least twice the cat's width and end on a free spot", () => {
    expect(MIN_WALK).toBeGreaterThanOrEqual(CAT_W * 2);
    for (const rest of [hybridRest(), catRest()]) {
      for (const x of spots(rest)) {
        for (const r of RNDS) {
          const t = pickWalk(x, rest, MIN_WALK, () => r);
          expect(t).not.toBeNull();
          expect(Math.abs(t! - x)).toBeGreaterThanOrEqual(MIN_WALK);
          expect(canRest(t!, rest)).toBe(true);
          expect(Number.isInteger(t)).toBe(true);
        }
      }
    }
  });

  test("hybrid walks never end on the clock: the cat crosses behind it", () => {
    const rest = hybridRest();
    for (const x of spots(rest)) {
      for (const r of RNDS) {
        const t = pickWalk(x, rest, MIN_WALK, () => r)!;
        expect(overlaps(t, CLOCK)).toBe(false);
        expect(Math.sign(t)).not.toBe(Math.sign(x));
      }
    }
  });

  test("every reachable spot can be picked", () => {
    const rest: Span[] = [[0, 9], [100, 109]];
    const seen = new Set<number>();
    for (let i = 0; i < 20; i++) seen.add(pickWalk(50, rest, 40, () => i / 20)!);
    expect(seen.size).toBe(20);
  });

  test("a narrow area falls back to its far end, a tiny one stays put", () => {
    expect(pickWalk(10, [[0, 30]], MIN_WALK, () => 0.5)).toBe(30);
    expect(pickWalk(4, [[0, 8]], MIN_WALK, () => 0.5)).toBeNull();
    expect(pickWalk(0, [], MIN_WALK, () => 0.5)).toBeNull();
  });

  test("music art and a dual clock are avoided too", () => {
    const rest = restSpans(128, [[-128, -100], CLOCK, [22, 60], [100, 128]], CAT_W, CAT_GAP, CAT_EDGE);
    expect(rest).toEqual([[-96, -50], [64, 70]]);
    for (const x of spots(rest)) for (const r of RNDS) {
      const t = pickWalk(x, rest, MIN_WALK, () => r);
      if (t !== null) expect(canRest(t, rest)).toBe(true);
    }
  });

  test("a cat caught behind the clock steps out to the nearest side", () => {
    const rest = hybridRest();
    expect(nearestRest(-30, rest)).toBe(-50);
    expect(nearestRest(15, rest)).toBe(24);
    expect(nearestRest(-60, rest)).toBe(-60);
    expect(nearestRest(200, rest)).toBe(68);
    expect(nearestRest(0, [])).toBeNull();
  });

  test("blocked spans merge and become a mask the cat walks behind", () => {
    expect(mergeSpans([[5, 9], [-3, 2], [1, 6]])).toEqual([[-3, 9]]);
    expect(behindMask([])).toBeUndefined();
    const mask = behindMask([CLOCK])!;
    expect(mask).toContain("transparent calc(50% - 20px)");
    expect(mask).toContain("transparent calc(50% + 20px)");
    expect(mask.startsWith("linear-gradient(to right, #000 calc(50% - 20px)")).toBe(true);
  });
});

describe("cat roaming, random layouts", () => {
  test("a walk always ends inside the area and clear of every blocked span", () => {
    let seed = 7;
    const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32);
    for (let i = 0; i < 300; i++) {
      const half = 40 + rnd() * 100;
      const blocked: Span[] = Array.from({ length: Math.floor(rnd() * 4) }, () => {
        const a = -half + rnd() * 2 * half;
        return [a, a + 5 + rnd() * 50] as Span;
      });
      const rest = restSpans(half, blocked, CAT_W, CAT_GAP, CAT_EDGE);
      for (const [s, e] of rest) {
        for (const x of [s, e]) {
          expect(x).toBeGreaterThanOrEqual(-half + CAT_EDGE - 1);
          expect(x + CAT_W).toBeLessThanOrEqual(half - CAT_EDGE + 1);
          for (const b of blocked) expect(overlaps(x, b)).toBe(false);
        }
      }
      const x = rest.length ? rest[0][0] : 0;
      const t = pickWalk(x, rest, MIN_WALK, rnd);
      if (t !== null) expect(canRest(t, rest)).toBe(true);
    }
  });
});
