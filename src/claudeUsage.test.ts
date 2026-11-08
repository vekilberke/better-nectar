import { describe, expect, test } from "bun:test";
import { liveWindows, parseUsage, parseUsageState, primaryWindow, resetIn, sameUsageState, usageLevel, usagePercent, type UsageState } from "./claudeUsage";

const NOW = 1_762_600_000_000;
const at = (secondsFromNow: number) => NOW / 1000 + secondsFromNow;
const H = 3_600_000;

const state = (s: Partial<UsageState>): UsageState => ({ line: null, lineAt: null, desktop: null, ...s });

describe("claude usage parsing (T-021)", () => {
  test("parses both status line windows and drops broken ones", () => {
    expect(parseUsage({ five_hour: { used_percentage: 42.4, resets_at: 100 }, seven_day: { used_percentage: 18, resets_at: 200 } })).toEqual({
      five_hour: { used_percentage: 42.4, resets_at: 100 },
      seven_day: { used_percentage: 18, resets_at: 200 },
    });
    expect(parseUsage({ five_hour: { used_percentage: "x", resets_at: 1 }, seven_day: { used_percentage: 3, resets_at: 9 } })).toEqual({
      seven_day: { used_percentage: 3, resets_at: 9 },
    });
    expect(parseUsage({ five_hour: { used_percentage: 120, resets_at: 1 } })?.five_hour?.used_percentage).toBe(100);
    expect(parseUsage({ five_hour: { used_percentage: 5, resets_at: 0 } })).toBeNull();
    expect(parseUsage({})).toBeNull();
    expect(parseUsage(null)).toBeNull();
    expect(parseUsage("five_hour")).toBeNull();
  });

  test("parses the command payload with either source missing", () => {
    expect(
      parseUsageState({ status_line: { five_hour: { used_percentage: 10, resets_at: 5 } }, status_line_at: 7, desktop: { five_hour: 12, seven_day: -3, at: 9 } })
    ).toEqual({ line: { five_hour: { used_percentage: 10, resets_at: 5 } }, lineAt: 7, desktop: { five_hour: 12, seven_day: 0, at: 9 } });
    expect(parseUsageState({ desktop: { at: 9 } })).toEqual({ line: null, lineAt: null, desktop: null });
    expect(parseUsageState({ desktop: { five_hour: 3, at: 0 } }).desktop).toBeNull();
    expect(parseUsageState(null)).toEqual({ line: null, lineAt: null, desktop: null });
  });
});

describe("live windows", () => {
  test("a status line window that has reset is gone", () => {
    const s = state({ line: parseUsage({ five_hour: { used_percentage: 80, resets_at: at(-1) }, seven_day: { used_percentage: 20, resets_at: at(3600) } }), lineAt: NOW - 1000 });
    const live = liveWindows(s, NOW);
    expect(live.map((w) => w.key)).toEqual(["seven_day"]);
    expect(live[0].left).toBe(H);
    expect(liveWindows(null, NOW)).toEqual([]);
  });

  test("a desktop sample alone shows the percentage without a reset time", () => {
    const s = state({ desktop: { five_hour: 11, seven_day: 38, at: NOW - 10 * 60_000 } });
    expect(liveWindows(s, NOW)).toEqual([
      { key: "five_hour", used_percentage: 11, left: null, at: NOW - 10 * 60_000, desktop: true },
      { key: "seven_day", used_percentage: 38, left: null, at: NOW - 10 * 60_000, desktop: true },
    ]);
  });

  test("a desktop 5-hour value is hidden after 30 minutes, the weekly one after 6 hours (T-024)", () => {
    const desktop = (ageMs: number) => state({ desktop: { five_hour: 70, seven_day: 40, at: NOW - ageMs } });
    expect(liveWindows(desktop(29 * 60_000), NOW).map((w) => w.key)).toEqual(["five_hour", "seven_day"]);
    expect(liveWindows(desktop(31 * 60_000), NOW).map((w) => w.key)).toEqual(["seven_day"]);
    expect(liveWindows(desktop(6 * H - 1), NOW).map((w) => w.key)).toEqual(["seven_day"]);
    expect(liveWindows(desktop(6 * H), NOW)).toEqual([]);
  });

  test("a desktop sample dated in the future was taken with a wrong clock and is hidden (T-024)", () => {
    const desktop = (aheadMs: number) => state({ desktop: { five_hour: 100, seven_day: 40, at: NOW + aheadMs } });
    expect(liveWindows(desktop(5 * 60_000), NOW).map((w) => w.key)).toEqual(["five_hour", "seven_day"]);
    expect(liveWindows(desktop(5 * 60_000 + 1), NOW)).toEqual([]);
    expect(liveWindows(desktop(3 * H), NOW)).toEqual([]);
  });

  test("a full 5-hour sample from hours ago does not show as the current limit (T-024)", () => {
    // The app's newest sample was taken at 20:27, the notch is looked at 23:04.
    const s = state({ desktop: { five_hour: 100, seven_day: 51, at: NOW - (2 * 60 + 37) * 60_000 } });
    expect(primaryWindow(s, NOW)).toEqual({ key: "seven_day", used_percentage: 51, left: null, at: NOW - (2 * 60 + 37) * 60_000, desktop: true });
  });

  test("an old desktop 5-hour sample does not beat a live status line window", () => {
    const line = parseUsage({ five_hour: { used_percentage: 10, resets_at: at(4.5 * 3600) } });
    expect(liveWindows(state({ line, lineAt: NOW, desktop: { five_hour: 40, at: NOW - 31 * 60_000 } }), NOW)[0]).toEqual({
      key: "five_hour",
      used_percentage: 10,
      left: 4.5 * H,
      at: NOW,
      desktop: false,
    });
  });

  test("within one window the higher source wins and a live status line keeps its reset time", () => {
    const line = parseUsage({ five_hour: { used_percentage: 40, resets_at: at(7200) } });
    const newerLine = state({ line, lineAt: NOW - 60_000, desktop: { five_hour: 30, at: NOW - 5 * 60_000 } });
    expect(liveWindows(newerLine, NOW)[0]).toEqual({ key: "five_hour", used_percentage: 40, left: 2 * H, at: NOW - 60_000, desktop: false });
    const newerDesktop = state({ line, lineAt: NOW - 30 * 60_000, desktop: { five_hour: 55, at: NOW - 5 * 60_000 } });
    expect(liveWindows(newerDesktop, NOW)[0]).toEqual({ key: "five_hour", used_percentage: 55, left: 2 * H, at: NOW - 5 * 60_000, desktop: true });
  });

  test("an old number with a fresh status line time does not beat a higher desktop sample (I-247)", () => {
    const line = parseUsage({ five_hour: { used_percentage: 20, resets_at: at(7200) } });
    expect(liveWindows(state({ line, lineAt: NOW, desktop: { five_hour: 60, at: NOW - 10 * 60_000 } }), NOW)[0]).toEqual({
      key: "five_hour",
      used_percentage: 60,
      left: 2 * H,
      at: NOW - 10 * 60_000,
      desktop: true,
    });
  });

  test("a stale status line time does not let a lower desktop sample win (I-246)", () => {
    const line = parseUsage({ five_hour: { used_percentage: 50, resets_at: at(7200) } });
    expect(liveWindows(state({ line, lineAt: NOW - H, desktop: { five_hour: 48, at: NOW - 60_000 } }), NOW)[0].used_percentage).toBe(50);
  });

  test("a desktop sample from before the live window began belongs to the previous window", () => {
    const line = parseUsage({ five_hour: { used_percentage: 10, resets_at: at(4 * 3600) } });
    expect(liveWindows(state({ line, lineAt: NOW, desktop: { five_hour: 80, at: NOW - 2 * H } }), NOW)[0].used_percentage).toBe(10);
    expect(liveWindows(state({ line, lineAt: NOW, desktop: { five_hour: 12, at: NOW - 20 * 60_000 } }), NOW)[0].used_percentage).toBe(12);
  });

  test("a desktop sample from before a known reset is dropped, one after it is kept", () => {
    const line = parseUsage({ five_hour: { used_percentage: 90, resets_at: at(-600) } });
    expect(liveWindows(state({ line, lineAt: NOW - H, desktop: { five_hour: 85, at: NOW - 900_000 } }), NOW)).toEqual([]);
    expect(liveWindows(state({ line, lineAt: NOW - H, desktop: { five_hour: 4, at: NOW - 60_000 } }), NOW)[0]).toEqual({
      key: "five_hour",
      used_percentage: 4,
      left: null,
      at: NOW - 60_000,
      desktop: true,
    });
  });

  test("the meter shows the window closest to its limit, the 5-hour one on a tie", () => {
    const s = state({ line: parseUsage({ five_hour: { used_percentage: 30, resets_at: at(60) }, seven_day: { used_percentage: 81, resets_at: at(600) } }), lineAt: NOW });
    expect(primaryWindow(s, NOW)?.key).toBe("seven_day");
    const tie = state({ desktop: { five_hour: 50, seven_day: 50, at: NOW } });
    expect(primaryWindow(tie, NOW)?.key).toBe("five_hour");
    expect(primaryWindow(null, NOW)).toBeNull();
  });
});

describe("formatting", () => {
  test("levels and shown percentages", () => {
    expect([usageLevel(0), usageLevel(74.9), usageLevel(75), usageLevel(89), usageLevel(90)]).toEqual(["ok", "ok", "warn", "warn", "high"]);
    expect([usagePercent(42.4), usagePercent(99.6), usagePercent(100)]).toEqual([42, 99, 100]);
  });

  test("time to reset uses the two largest units and rounds up", () => {
    expect(resetIn(1)).toEqual({ unit: "min", m: 1 });
    expect(resetIn(59 * 60_000)).toEqual({ unit: "min", m: 59 });
    expect(resetIn((2 * 60 + 13) * 60_000 - 5_000)).toEqual({ unit: "hour", h: 2, m: 13 });
    expect(resetIn((3 * 24 + 4) * H)).toEqual({ unit: "day", d: 3, h: 4 });
    expect(resetIn(-5)).toEqual({ unit: "min", m: 1 });
  });

  test("sameUsageState compares every field", () => {
    const a = parseUsageState({ status_line: { five_hour: { used_percentage: 1, resets_at: 2 } }, status_line_at: 3, desktop: { seven_day: 4, at: 5 } });
    expect(sameUsageState(a, parseUsageState({ status_line: { five_hour: { used_percentage: 1, resets_at: 2 } }, status_line_at: 3, desktop: { seven_day: 4, at: 5 } }))).toBe(true);
    expect(sameUsageState(a, parseUsageState({ status_line: { five_hour: { used_percentage: 1, resets_at: 9 } }, status_line_at: 3, desktop: { seven_day: 4, at: 5 } }))).toBe(false);
    expect(sameUsageState(a, parseUsageState({ status_line: { five_hour: { used_percentage: 1, resets_at: 2 } }, status_line_at: 3, desktop: { seven_day: 6, at: 5 } }))).toBe(false);
    expect(sameUsageState(state({}), state({}))).toBe(true);
  });
});

describe("edge cases (tester round 1)", () => {
  test("reset time unit boundaries", () => {
    expect(resetIn(60 * 60_000)).toEqual({ unit: "hour", h: 1, m: 0 });
    expect(resetIn(24 * 60 * 60_000)).toEqual({ unit: "day", d: 1, h: 0 });
    expect(resetIn(24 * 60 * 60_000 - 60_000)).toEqual({ unit: "hour", h: 23, m: 59 });
    expect(resetIn(24 * 60 * 60_000 - 1)).toEqual({ unit: "day", d: 1, h: 0 });
  });

  test("a desktop 5-hour sample exactly 30 minutes old is dropped, one ms younger is kept", () => {
    expect(liveWindows(state({ desktop: { five_hour: 50, at: NOW - 30 * 60_000 } }), NOW)).toEqual([]);
    expect(liveWindows(state({ desktop: { five_hour: 50, at: NOW - 30 * 60_000 + 1 } }), NOW)).toHaveLength(1);
  });

  test("a live status line without a timestamp still shows and a newer desktop sample then wins the number", () => {
    const line = parseUsage({ five_hour: { used_percentage: 40, resets_at: at(600) } });
    expect(liveWindows(state({ line, lineAt: null }), NOW)[0].left).toBe(600_000);
    expect(liveWindows(state({ line, lineAt: null, desktop: { five_hour: 45, at: NOW } }), NOW)[0].used_percentage).toBe(45);
  });

  test("a window resetting exactly now is gone", () => {
    const line = parseUsage({ five_hour: { used_percentage: 99, resets_at: at(0) } });
    expect(liveWindows(state({ line, lineAt: NOW }), NOW)).toEqual([]);
  });

  test("shown percentage never reads 100 below the limit and handles tiny values", () => {
    expect([usagePercent(0), usagePercent(0.4), usagePercent(99.4), usagePercent(99.5)]).toEqual([0, 0, 99, 99]);
  });
});
