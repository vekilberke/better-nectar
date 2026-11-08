import { describe, expect, test } from "bun:test";
import { usageTooltip } from "./UsageMeter";
import { parseUsage, type UsageState } from "../claudeUsage";

const t = (key: string, params?: Record<string, string | number>) =>
  key + (params ? `(${Object.entries(params).map(([k, v]) => `${k}=${v}`).join(",")})` : "");
const MIN = 60_000;
const state = (p: Partial<UsageState>): UsageState => ({ line: null, lineAt: null, desktop: null, ...p });

describe("usage tooltip (T-024)", () => {
  const now = Date.now();
  const line = parseUsage({ five_hour: { used_percentage: 40, resets_at: now / 1000 + 7200 } });

  test("says the age when a desktop sample beats a live status line window", () => {
    const s = state({ line, lineAt: now, desktop: { five_hour: 55, at: now - 5 * MIN } });
    const text = usageTooltip(t, s, now);
    expect(text).toContain("usage.line(");
    expect(text).toContain("usage.fromApp(ago=common.ago.minutes(count=5))");
  });

  test("does not mention the app when the status line value is shown", () => {
    const s = state({ line, lineAt: now, desktop: { five_hour: 30, at: now - 5 * MIN } });
    expect(usageTooltip(t, s, now)).not.toContain("usage.fromApp");
  });

  test("a desktop-only window uses the as-of line with its age, and a stale 5-hour one is absent", () => {
    const s = state({ desktop: { five_hour: 100, seven_day: 51, at: now - 157 * MIN } });
    const text = usageTooltip(t, s, now);
    expect(text).toContain("usage.lineAsOf(window=usage.window.seven_day");
    expect(text).not.toContain("five_hour");
  });
});
