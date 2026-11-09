import { describe, expect, test } from "bun:test";
import { AGENT_BAR_MAX, AGENT_BAR_MIN, agentBarRunKey, agentBarWidth, agentTimerTemplate, stickyWidth, STICKY_HOLD_MS, type StickyWidth } from "./agentBarLayout";
import { createAgentSessions } from "./agentSessions";
import type { AgentActivity } from "./components/AgentStatus";

describe("agent bar width (T-017)", () => {
  test("sums icon, status, timer and padding with no slack in the middle", () => {
    expect(agentBarWidth({ status: 100, time: 30 })).toBe(14 * 2 + 16 + 8 + 100 + 8 + 30 + 6);
  });

  test("a count badge adds its width and one gap", () => {
    const one = agentBarWidth({ status: 100, time: 30 });
    expect(agentBarWidth({ status: 100, time: 30, count: 7 })).toBe(one + 16 + 8);
  });

  test("the usage ring adds its width and one gap", () => {
    const one = agentBarWidth({ status: 100, time: 30 });
    expect(agentBarWidth({ status: 100, time: 30, meter: true })).toBe(one + 12 + 8);
  });

  test("short and long statuses are clamped", () => {
    expect(agentBarWidth({ status: 10, time: 20 })).toBe(AGENT_BAR_MIN);
    expect(agentBarWidth({ status: 900, time: 30, count: 6 })).toBe(AGENT_BAR_MAX);
  });

  test("is much narrower than the old fixed 270 for a plain working status", () => {
    expect(agentBarWidth({ status: 52, time: 30 })).toBeLessThan(200);
  });

  test("timer template grows after an hour", () => {
    expect(agentTimerTemplate(59 * 60_000)).toBe("00:00");
    expect(agentTimerTemplate(3_600_000)).toBe("0:00:00");
  });

  test("width holds within a run and resets for a new one", () => {
    let s = stickyWidth(null, 1, 180, 0);
    s = stickyWidth(s, 1, 150, 100);
    expect(s.width).toBe(180);
    s = stickyWidth(s, 1, 210, 200);
    expect(s.width).toBe(210);
    s = stickyWidth(s, 2, 150, 300);
    expect(s).toEqual({ run: 2, width: 150, seen: 300 });
  });

  test("a held width shrinks to fit once the wide content has been gone for the hold time", () => {
    let s = stickyWidth(null, 1, 300, 0);
    s = stickyWidth(s, 1, 160, 1000);
    expect(s.width).toBe(300);
    s = stickyWidth(s, 1, 160, STICKY_HOLD_MS - 1);
    expect(s.width).toBe(300);
    s = stickyWidth(s, 1, 160, STICKY_HOLD_MS);
    expect(s.width).toBe(160);
  });

  test("wide content coming back restarts the hold", () => {
    let s = stickyWidth(null, 1, 300, 0);
    s = stickyWidth(s, 1, 300, 2500);
    s = stickyWidth(s, 1, 160, 4000);
    expect(s.width).toBe(300);
    s = stickyWidth(s, 1, 160, 2500 + STICKY_HOLD_MS);
    expect(s.width).toBe(160);
  });
  test("a count of zero or an empty status still gives a width inside the bounds", () => {
    expect(agentBarWidth({ status: 0, time: 0, count: 0 })).toBe(AGENT_BAR_MIN);
    const w = agentBarWidth({ status: 0, time: 0, count: 1 });
    expect(w).toBeGreaterThanOrEqual(AGENT_BAR_MIN);
    expect(w).toBeLessThanOrEqual(AGENT_BAR_MAX);
  });

  test("a tiny count badge keeps its 16px minimum", () => {
    const base = agentBarWidth({ status: 100, time: 30 });
    expect(agentBarWidth({ status: 100, time: 30, count: 1 })).toBe(base + 16 + 8);
    expect(agentBarWidth({ status: 100, time: 30, count: 20 })).toBe(base + 28 + 8);
  });

  test("fractional text widths round up to a whole pixel", () => {
    const w = agentBarWidth({ status: 100.2, time: 30.3 });
    expect(Number.isInteger(w)).toBe(true);
    expect(w).toBe(Math.ceil(14 * 2 + 16 + 8 + 100.2 + 8 + 30.3 + 6));
  });

  test("width never shrinks as the status grows and stops at the maximum", () => {
    let prev = 0;
    for (const status of [40, 80, 120, 160, 200, 400]) {
      const w = agentBarWidth({ status, time: 30 });
      expect(w).toBeGreaterThanOrEqual(prev);
      prev = w;
    }
    expect(prev).toBe(AGENT_BAR_MAX);
  });

  test("the timer template switches exactly at one hour and handles bad input", () => {
    expect(agentTimerTemplate(3_599_999)).toBe("00:00");
    expect(agentTimerTemplate(0)).toBe("00:00");
    expect(agentTimerTemplate(-5)).toBe("00:00");
    expect(agentTimerTemplate(100 * 3_600_000)).toBe("0:00:00");
  });

  test("an equal width refreshes the hold and a new run can shrink", () => {
    const a = stickyWidth(null, 5, 200, 0);
    expect(stickyWidth(a, 5, 200, 50)).toEqual({ run: 5, width: 200, seen: 50 });
    expect(stickyWidth(a, 6, 150, 50)).toEqual({ run: 6, width: 150, seen: 50 });
  });

  test("the width does not pulse while focus alternates between concurrent sessions (I-175)", () => {
    const store = createAgentSessions();
    const fit: Record<string, number> = { a: 220, b: 160 };
    const act = (session: string, over: Partial<AgentActivity> = {}): AgentActivity => ({ state: "working", name: "Claude Code", project: "nectar", session, ...over });
    store.update(act("a", { state: "tool", tool: "Bash", fresh: true }), 0);
    store.update(act("b", { fresh: true }), 10);
    let sticky: StickyWidth | null = null;
    const widths: number[] = [];
    const focused = new Set<string>();
    for (let i = 0; i < 6; i++) {
      const session = i % 2 ? "b" : "a";
      store.update(act(session, session === "a" ? { state: "tool", tool: "Bash" } : {}), 100 + i * 10);
      const view = store.view(100 + i * 10);
      focused.add(view.focus!.activity.session!);
      sticky = stickyWidth(sticky, agentBarRunKey(view.run), fit[view.focus!.activity.session!], 100 + i * 10);
      widths.push(sticky.width);
    }
    expect(focused.size).toBe(2);
    expect(new Set(widths)).toEqual(new Set([220]));
  });

  test("one session keys by its run, several share one key", () => {
    expect(agentBarRunKey({ count: 1, since: 5 })).toBe(5);
    expect(agentBarRunKey({ count: 0, since: 5 })).toBe(5);
    expect(agentBarRunKey({ count: 2, since: 5 })).toBe(agentBarRunKey({ count: 3, since: 9 }));
  });
});
