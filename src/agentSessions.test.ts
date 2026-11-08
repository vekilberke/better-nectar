import { describe, expect, test } from "bun:test";
import { createAgentSessions, agentGroup, summarizeAgents, canOpenAgent, ERROR_GRACE_MS, DONE_SHOW_MS, STALE_MS } from "./agentSessions";
import type { AgentActivity } from "./components/AgentStatus";

const dropNullsPayload = (over: Record<string, unknown>) => ({ name: "Claude Code", project: "nectar", session: "a", ...over }) as unknown as AgentActivity;
const act = (over: Partial<AgentActivity>): AgentActivity => ({ state: "working", name: "Claude Code", project: "nectar", session: "a", ...over });

describe("agent session state machine", () => {
  test("a later done sets the finish time even after an error (I-016)", () => {
    const s = createAgentSessions();
    s.update(act({ state: "working", fresh: true }), 1000);
    s.update(act({ state: "error", tool: "Bash" }), 2000);
    expect(s.list(2000)[0].end).toBe(2000);
    s.update(act({ state: "done" }), 5000);
    const [done] = s.list(5000);
    expect(done.end).toBe(5000);
    expect(done.since).toBe(1000);
  });

  test("working after an error within the grace keeps the run start", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    s.update(act({ state: "error" }), 2000);
    const resumed = s.update(act({ state: "working" }), 3000);
    expect(resumed.since).toBe(1000);
    expect(resumed.end).toBeUndefined();
  });

  test("a fresh prompt starts a new run", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    s.update(act({ state: "done" }), 2000);
    const next = s.update(act({ fresh: true }), 9000);
    expect(next.since).toBe(9000);
  });

  test("title is kept from the first prompt and never replaced", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true, title: "Fix the notch" }), 1000);
    s.update(act({ state: "tool", tool: "Edit" }), 1500);
    const later = s.update(act({ fresh: true, title: "Something else" }), 2000);
    expect(later.title).toBe("Fix the notch");
  });

  test("window and project survive events that lack them", () => {
    const s = createAgentSessions();
    s.update(act({ window: 42 }), 1000);
    const next = s.update(act({ state: "done", window: undefined, project: undefined }), 2000);
    expect(next.activity.window).toBe(42);
    expect(next.activity.project).toBe("nectar");
  });

  test("a pending ask is not overwritten by tool events, but a cancel or answer clears it", () => {
    const s = createAgentSessions();
    s.update(act({ state: "needs_input", ask: { id: 7, tool: "Bash", detail: "ls" } }), 1000);
    s.update(act({ state: "tool", tool: "Read" }), 1500);
    expect(s.list(1500)[0].activity.ask?.id).toBe(7);
    s.update(act({ state: "working", cancelled: 7 }), 2000);
    expect(s.list(2000)[0].activity.ask).toBeUndefined();

    s.update(act({ state: "needs_input", ask: { id: 8 } }), 3000);
    s.answer("a", 3500);
    const [row] = s.list(3500);
    expect(row.activity.state).toBe("working");
    expect(row.activity.ask).toBeUndefined();
  });

  test("an ask in one session outranks work in a newer one (I-017)", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a", state: "needs_input", ask: { id: 1, tool: "Bash" } }), 1000);
    s.update(act({ session: "b", state: "working", fresh: true }), 2000);
    const view = s.view(2000);
    expect(view.focus?.key).toBe("a");
    expect(view.count).toBe(2);
  });

  test("done and error expire per session without touching the others (I-019)", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a", state: "done" }), 0);
    s.update(act({ session: "b", state: "error" }), 0);
    s.update(act({ session: "c", fresh: true }), 0);
    expect(s.view(DONE_SHOW_MS - 1).focus?.key).toBe("b");
    expect(s.view(DONE_SHOW_MS + 1).count).toBe(2);
    expect(s.view(ERROR_GRACE_MS + 1).focus?.key).toBe("c");
    expect(s.view(ERROR_GRACE_MS + 1).count).toBe(1);
    expect(s.list(ERROR_GRACE_MS + 1)).toHaveLength(3);
  });

  test("count is recomputed from time, so stale errors drop out (I-020)", () => {
    const s = createAgentSessions();
    s.update(act({ state: "error" }), 0);
    expect(s.view(1000).count).toBe(1);
    expect(s.view(ERROR_GRACE_MS + 1).count).toBe(0);
  });

  test("nextExpiry points at the soonest done or error expiry", () => {
    const s = createAgentSessions();
    expect(s.nextExpiry(0)).toBeNull();
    s.update(act({ session: "a", state: "error" }), 0);
    s.update(act({ session: "b", state: "done" }), 0);
    expect(s.nextExpiry(1)).toBe(DONE_SHOW_MS);
    expect(s.nextExpiry(DONE_SHOW_MS + 1)).toBe(ERROR_GRACE_MS);
  });

  test("a manual session is ended by a hook session end from the same window (I-021)", () => {
    const s = createAgentSessions();
    s.update({ state: "working", name: "Claude Code", project: "nectar", window: 99 }, 1000);
    expect(s.list(1000)).toHaveLength(1);
    s.end({ session: "abc", window: 99, name: "Claude Code", project: "nectar" });
    expect(s.list(1000)).toHaveLength(0);
  });

  test("a manual clear without a session ends the window's sessions, other windows stay", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a", window: 1 }), 1000);
    s.update(act({ session: "b", window: 2 }), 1000);
    expect(s.end({ window: 1, name: "Claude Code", project: "nectar" })).toBe(1);
    expect(s.list(1000).map((r) => r.key)).toEqual(["b"]);
  });

  test("ending one hook session leaves other hook sessions alone", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a", window: 1 }), 1000);
    s.update(act({ session: "b", window: 1 }), 1000);
    s.end({ session: "a", window: 1 });
    expect(s.list(1000).map((r) => r.key)).toEqual(["b"]);
  });

  test("dismiss hides from the notch status but keeps the row; a new event shows it again", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    s.dismiss("a");
    expect(s.view(1500).focus).toBeNull();
    expect(s.list(1500)).toHaveLength(1);
    s.update(act({ state: "tool", tool: "Bash" }), 2000);
    expect(s.view(2000).focus?.key).toBe("a");
  });

  test("clearFinished removes done and error rows only", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a", state: "done" }), 0);
    s.update(act({ session: "b", state: "error" }), 0);
    s.update(act({ session: "c", fresh: true }), 0);
    s.update(act({ session: "d", state: "needs_input", ask: { id: 2 } }), 0);
    expect(s.clearFinished()).toBe(2);
    expect(s.list(0).map((r) => r.key).sort()).toEqual(["c", "d"]);
  });

  test("list orders waiting, working, failed, then done", () => {
    const s = createAgentSessions();
    s.update(act({ session: "done", state: "done" }), 1);
    s.update(act({ session: "work", fresh: true }), 2);
    s.update(act({ session: "err", state: "error" }), 3);
    s.update(act({ session: "wait", state: "needs_input", ask: { id: 1 } }), 4);
    expect(s.list(5).map((r) => r.key)).toEqual(["wait", "work", "err", "done"]);
  });

  test("sessions unseen for hours are pruned", () => {
    const s = createAgentSessions();
    s.update(act({ session: "old", fresh: true }), 0);
    expect(s.list(STALE_MS + 1)).toHaveLength(0);
  });

  test("groups and summary", () => {
    expect(agentGroup({ state: "needs_input", idle: true })).toBe("done");
    expect(agentGroup({ state: "needs_input" })).toBe("waiting");
    expect(agentGroup({ state: "tool" })).toBe("working");
    const s = createAgentSessions();
    s.update(act({ session: "a", state: "done" }), 0);
    s.update(act({ session: "b", state: "needs_input", idle: true }), 0);
    s.update(act({ session: "c", fresh: true }), 0);
    expect(summarizeAgents(s.list(0))).toEqual({ waiting: 0, working: 1, done: 2, error: 0 });
  });
  test("a title arriving after a title-less start is adopted (tester)", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    expect(s.update(act({ fresh: true, title: "Late title" }), 2000).title).toBe("Late title");
  });

  test("ending an unknown session removes nothing (tester)", () => {
    const s = createAgentSessions();
    s.update(act({ session: "a" }), 0);
    expect(s.end({ session: "zzz", name: "Other", project: "other" })).toBe(0);
    expect(s.list(0).length).toBe(1);
  });

  test("answering an unknown key is a no-op and clearFinished keeps a waiting ask (tester)", () => {
    const s = createAgentSessions();
    s.answer("nope", 0);
    s.update(act({ session: "w", state: "needs_input", ask: { id: 1, tool: "Bash" } }), 0);
    expect(s.clearFinished()).toBe(0);
    expect(s.list(0)[0].activity.ask?.id).toBe(1);
  });

  test("null-valued payloads from Rust behave like missing fields (I-021)", () => {
    const store = createAgentSessions();
    const nul = (o: object) => o as never;
    store.update(nul({ state: "working", name: "Claude Code", project: null, window: 42, session: null }), 1000);
    store.update(nul({ state: "working", name: "Claude Code", session: "abc", window: 42 }), 1001);
    expect(store.list(1002).length).toBe(2);
    expect(store.end(nul({ session: "abc", window: 42, name: null, project: null }))).toBe(2);
    expect(store.list(1003).length).toBe(0);
  });
  test("end with window null falls back to name and project", () => {
    const store = createAgentSessions();
    store.update({ state: "working", name: "A", project: "p", window: 7 } as AgentActivity, 1000);
    expect(store.end({ window: null, name: "A", project: "p" } as never)).toBe(1);
  });
  test("nextExpiry and view ignore dismissed sessions; clearFinished still removes them (tester round 2)", () => {
    const store = createAgentSessions();
    store.update(act({ session: "a", state: "done" }), 0);
    store.dismiss("a");
    expect(store.nextExpiry(1)).toBeNull();
    expect(store.view(1).focus).toBeNull();
    expect(store.list(1).length).toBe(1);
    expect(store.clearFinished()).toBe(1);
  });
  test("a done event while a permission ask is pending replaces the ask (known limit)", () => {
    const store = createAgentSessions();
    store.update(act({ state: "needs_input", ask: { id: 1, tool: "Bash" } } as Partial<AgentActivity>), 0);
    store.update(act({ state: "done" }), 5);
    expect(store.list(5)[0].activity.ask).toBeUndefined();
  });

  test("the Claude desktop session id survives events that lack it (T-017)", () => {
    const s = createAgentSessions();
    s.update(act({ host: "local_abc" }), 1000);
    expect(s.update(act({ state: "tool", tool: "Bash" }), 2000).activity.host).toBe("local_abc");
    expect(s.update(dropNullsPayload({ state: "done", host: null }), 3000).activity.host).toBe("local_abc");
  });

  test("a session opens by window, Claude Code session or desktop session (T-017)", () => {
    expect(canOpenAgent({})).toBe(false);
    expect(canOpenAgent({ window: 7 })).toBe(true);
    expect(canOpenAgent({ session: "uuid" })).toBe(true);
    expect(canOpenAgent({ host: "local_x" })).toBe(true);
  });
});
