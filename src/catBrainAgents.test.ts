import { readFileSync } from "node:fs";
import { describe, expect, test } from "bun:test";
import {
  AGENT_CAT_START, agentCue, agentMood, agentSound, catSpritePx, CHEER_MS, CUE_FRESH_MS, MEOW_MS, REACT_GAP_MS, STARTLE_MS, stepAgentCat,
  type AgentCatState, type CatCue,
} from "./catBrain";
import { createAgentSessions } from "./agentSessions";
import { agentBarWidth } from "./agentBarLayout";
import type { AgentActivity } from "./components/AgentStatus";
import { CAT_W } from "./components/sonderCatData";

const act = (over: Partial<AgentActivity>): AgentActivity => ({ state: "working", name: "Claude Code", project: "nectar", session: "a", ...over });

/** Steps the cat through cues like the component does: on every cue and when a one-shot ends. */
function run(cues: [number, CatCue][], until: number) {
  let s: AgentCatState = AGENT_CAT_START;
  const poses: [number, string][] = [];
  let meows = 0;
  let i = 0;
  let cue: CatCue | null = null;
  for (let now = 0; now <= until; now += 50) {
    let changed = false;
    while (i < cues.length && cues[i][0] <= now) { cue = cues[i++][1]; changed = true; }
    if (!cue || (!changed && !(s.until && now >= s.until))) continue;
    const next = stepAgentCat(s, cue, now);
    if (next.last.meow !== s.last.meow) meows++;
    s = next;
    if (poses[poses.length - 1]?.[1] !== s.pose) poses.push([now, s.pose]);
  }
  return { poses, meows, state: s };
}

describe("cat and AI agents: priority", () => {
  test("the shown session's mood follows ask > needs input > error > working > done", () => {
    const s = createAgentSessions();
    const mood = (now: number) => agentMood(s.view(now).focus!.activity);
    s.update(act({ session: "d", state: "done" }), 1000);
    expect(mood(1000)).toBe("done");
    s.update(act({ session: "w", state: "tool", tool: "Bash" }), 1100);
    expect(mood(1100)).toBe("working");
    s.update(act({ session: "e", state: "error" }), 1200);
    expect(mood(1200)).toBe("error");
    s.update(act({ session: "n", state: "needs_input" }), 1300);
    expect(mood(1300)).toBe("waiting");
    s.update(act({ session: "q", state: "needs_input", ask: { id: 7, tool: "Bash" } }), 1400);
    expect(mood(1400)).toBe("ask");
    s.update(act({ session: "d", state: "done" }), 1500);
    expect(mood(1500)).toBe("ask");
  });

  test("an idle prompt is not news: it keeps the finished session's mood and cue", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    const done = agentCue(s.update(act({ state: "done" }), 2000));
    const idle = agentCue(s.update(act({ state: "needs_input", idle: true }), 62000));
    expect(idle.mood).toBe("done");
    expect(idle.id).toBe(done.id);
    expect(idle.at).toBe(2000);
  });

  test("every finish and every ask is its own news; working is one steady cue", () => {
    const s = createAgentSessions();
    s.update(act({ fresh: true }), 1000);
    const first = agentCue(s.update(act({ state: "done" }), 2000));
    s.update(act({ fresh: true }), 3000);
    expect(agentCue(s.update(act({ state: "tool", tool: "Edit" }), 3500)).id).toBe("working");
    const second = agentCue(s.update(act({ state: "done" }), 4000));
    expect(second.id).not.toBe(first.id);
    const ask1 = agentCue(s.update(act({ state: "needs_input", ask: { id: 1 } }), 5000));
    const ask2 = agentCue(s.update(act({ state: "needs_input", ask: { id: 2 } }), 6000));
    expect(ask1.id).not.toBe(ask2.id);
  });
});

describe("cat and AI agents: reactions", () => {
  const working: CatCue = { mood: "working", id: "working", at: 0 };

  test("working types along", () => {
    expect(stepAgentCat(AGENT_CAT_START, working, 0).pose).toBe("work");
  });

  test("a fresh finish cheers for CHEER_MS, then the cat sits content", () => {
    const { poses } = run([[0, working], [1000, { mood: "done", id: "done:a:1000", at: 1000 }]], 10000);
    expect(poses).toEqual([[0, "work"], [1000, "cheer"], [1000 + CHEER_MS, "content"]]);
  });

  test("an ask meows once, then holds the attention pose until answered", () => {
    const ask: CatCue = { mood: "ask", id: "ask:a:1", at: 2000 };
    const { poses, meows } = run([[0, working], [2000, ask], [9000, working]], 12000);
    expect(meows).toBe(1);
    expect(poses).toEqual([[0, "work"], [2000, "meow"], [2000 + MEOW_MS, "alert"], [9000, "work"]]);
  });

  test("needs input meows like an ask", () => {
    const r = stepAgentCat(AGENT_CAT_START, { mood: "waiting", id: "waiting:a", at: 0 }, 0);
    expect(r.pose).toBe("meow");
    expect(r.until).toBe(MEOW_MS);
  });

  test("an error startles the cat, then it stays sad", () => {
    const { poses } = run([[0, working], [500, { mood: "error", id: "error:a", at: 500 }]], 5000);
    expect(poses).toEqual([[0, "work"], [500, "startle"], [500 + STARTLE_MS, "sad"]]);
  });

  test("old news only holds the pose: no cheer or meow for a stale cue", () => {
    const stale = CUE_FRESH_MS + 1;
    const done = stepAgentCat(AGENT_CAT_START, { mood: "done", id: "done:a:0", at: 0 }, stale);
    expect(done.pose).toBe("content");
    const ask = stepAgentCat(AGENT_CAT_START, { mood: "ask", id: "ask:a:1", at: 0 }, stale);
    expect(ask.last.meow).toBeUndefined();
    expect(ask.pose).toBe("alert");
  });

  test("the same cue again changes nothing, so nothing re-renders", () => {
    const cue: CatCue = { mood: "done", id: "done:a:0", at: 0 };
    const a = stepAgentCat(AGENT_CAT_START, cue, 0);
    expect(stepAgentCat(a, cue, 100)).toBe(a);
    const held = stepAgentCat(a, cue, CHEER_MS + 10);
    expect(held.pose).toBe("content");
    expect(stepAgentCat(held, cue, CHEER_MS + 500)).toBe(held);
  });

  test("a higher-priority ask cuts a cheer short", () => {
    const { poses } = run([[0, { mood: "done", id: "done:a:0", at: 0 }], [800, { mood: "ask", id: "ask:b:1", at: 800 }]], 4000);
    expect(poses.slice(0, 2)).toEqual([[0, "cheer"], [800, "meow"]]);
  });
});

describe("cat and AI agents: no reaction spam", () => {
  test("ten sessions finishing within seconds start one cheer, which plays out once", () => {
    const cues: [number, CatCue][] = Array.from({ length: 10 }, (_, i) => [i * 300, { mood: "done", id: `done:s${i}:${i * 300}`, at: i * 300 }]);
    const { poses } = run(cues, 8000);
    expect(poses).toEqual([[0, "cheer"], [CHEER_MS, "content"]]);
  });

  test("asks flipping between sessions meow at most once per REACT_GAP_MS", () => {
    const cues: [number, CatCue][] = [];
    for (let t = 0; t < 20000; t += 700) cues.push([t, { mood: "ask", id: `ask:s${t % 3}:${t}`, at: t }]);
    const { meows } = run(cues, 20000);
    expect(meows).toBe(Math.ceil(20000 / REACT_GAP_MS));
  });

  test("working sessions taking turns on the notch never restart anything", () => {
    let s = stepAgentCat(AGENT_CAT_START, { mood: "working", id: "working", at: 0 }, 0);
    for (let t = 100; t < 5000; t += 100) {
      const next = stepAgentCat(s, { mood: "working", id: "working", at: t }, t);
      expect(next).toBe(s);
      s = next;
    }
  });

  test("a finish right after the gap cheers again", () => {
    const { poses } = run([[0, { mood: "done", id: "done:a:0", at: 0 }], [REACT_GAP_MS + 100, { mood: "done", id: "done:b:1", at: REACT_GAP_MS + 100 }]], REACT_GAP_MS + 4000);
    expect(poses.filter(([, p]) => p === "cheer").length).toBe(2);
  });
});

describe("cat and AI agents: sound", () => {
  /** Feeds updates through agentSound like App's listener: one last key per session. */
  function sounds(updates: [AgentActivity, boolean][]) {
    const last = new Map<string, string>();
    return updates.map(([a, catShown]) => {
      const r = agentSound(a, last.get(a.session ?? ""), catShown);
      last.set(a.session ?? "", r.key);
      return r.state ? (r.meow ? "meow" : r.state) : null;
    });
  }
  const ask = (session: string, id: number) => act({ session, state: "needs_input", ask: { id, tool: "Bash" } });

  test("back-to-back asks of one session each make a sound, however close together", () => {
    expect(sounds([[ask("a", 1), true], [act({ session: "a", cancelled: 1 }), true], [ask("a", 2), true]])).toEqual(["meow", null, "meow"]);
  });

  test("two sessions asking seconds apart both make a sound", () => {
    expect(sounds([[ask("a", 1), true], [ask("b", 1), true]])).toEqual(["meow", "meow"]);
  });

  test("a session starting to wait while another's ask is shown still makes a sound", () => {
    expect(sounds([[ask("a", 1), true], [act({ session: "b", state: "needs_input" }), true]])).toEqual(["meow", "meow"]);
  });

  test("the same update twice sounds once", () => {
    expect(sounds([[ask("a", 1), true], [ask("a", 1), true]])).toEqual(["meow", null]);
  });

  test("a new ask always makes a sound: the cat only picks meow or chime", () => {
    expect(sounds([[ask("a", 1), false], [ask("b", 1), true]])).toEqual(["needs_input", "meow"]);
  });

  test("idle prompts, finishes and errors keep their own sounds; working is silent", () => {
    expect(sounds([
      [act({ session: "a", state: "needs_input", idle: true }), true],
      [act({ session: "b", state: "done" }), true],
      [act({ session: "c", state: "error" }), true],
      [act({ session: "d", state: "tool", tool: "Edit" }), true],
      [act({ session: "e", state: "working" }), true],
    ])).toEqual(["needs_input", "done", "error", null, null]);
  });
});

describe("cat and AI agents: sound edge cases", () => {
  const key = (a: Parameters<typeof agentSound>[0], last?: string, shown = true) => agentSound(a, last, shown);

  test("ask id 0 is a real ask: its key differs from a plain wait and it meows", () => {
    const withZero = key(act({ session: "a", state: "needs_input", ask: { id: 0, tool: "Bash" } }));
    const plain = key(act({ session: "a", state: "needs_input" }));
    expect(withZero.key).not.toBe(plain.key);
    expect(withZero.meow).toBe(true);
  });

  test("an idle prompt that carries an ask still meows; a bare idle prompt does not", () => {
    expect(key(act({ session: "a", state: "needs_input", idle: true, ask: { id: 3, tool: "Bash" } })).meow).toBe(true);
    const bare = key(act({ session: "a", state: "needs_input", idle: true }));
    expect(bare.state).toBe("needs_input");
    expect(bare.meow).toBe(false);
  });

  test("nothing meows while the cat is not on the notch", () => {
    expect(key(act({ session: "a", state: "needs_input", ask: { id: 1, tool: "Bash" } }), undefined, false).meow).toBe(false);
  });

  test("the shipped meow is a valid short 16-bit mono wav that is not silent", () => {
    const b = readFileSync("public/sounds/cat-meow.wav");
    expect(b.toString("ascii", 0, 4)).toBe("RIFF");
    expect(b.toString("ascii", 8, 12)).toBe("WAVE");
    expect(b.readUInt16LE(22)).toBe(1);
    expect(b.readUInt16LE(34)).toBe(16);
    const dataLen = b.readUInt32LE(40);
    expect(dataLen).toBe(b.length - 44);
    const seconds = dataLen / 2 / b.readUInt32LE(24);
    expect(seconds).toBeGreaterThan(0.3);
    expect(seconds).toBeLessThan(1.5);
    let peak = 0;
    for (let i = 44; i + 1 < b.length; i += 2) peak = Math.max(peak, Math.abs(b.readInt16LE(i)));
    expect(peak).toBeGreaterThan(2000);
    expect(peak).toBeLessThan(32767);
  });
});

describe("cat and AI agents: compact bar", () => {
  test("the cat in place of the spark widens the bar by the difference only, within the clamp", () => {
    const parts = { status: 60, time: 34 };
    expect(agentBarWidth({ ...parts, icon: CAT_W }) - agentBarWidth(parts)).toBe(CAT_W - 16);
    expect(agentBarWidth({ status: 0, time: 0, icon: CAT_W })).toBe(140);
    expect(agentBarWidth({ status: 400, time: 34, icon: CAT_W })).toBe(300);
  });

  test("the bar reserves the width the cat is drawn at, not its full sprite width", () => {
    expect(catSpritePx(CAT_W, 1)).toBe(CAT_W);
    expect(catSpritePx(CAT_W, 2)).toBe(CAT_W);
    expect(catSpritePx(CAT_W, 1.25)).toBeCloseTo(20.8, 5);
    expect(catSpritePx(CAT_W, 1.5)).toBeCloseTo(17.333, 3);
    const parts = { status: 60, time: 34 };
    expect(agentBarWidth({ ...parts, icon: catSpritePx(CAT_W, 1.5) }) - agentBarWidth(parts)).toBeLessThanOrEqual(2);
    expect(agentBarWidth({ ...parts, icon: catSpritePx(CAT_W, 1.25) }) - agentBarWidth(parts)).toBeLessThanOrEqual(5);
  });
});

describe("cat and AI agents: boundaries", () => {
  const cue = (mood: CatCue["mood"], id: string, at: number): CatCue => ({ mood, id, at });

  test("news exactly CUE_FRESH_MS old still reacts, one ms older only holds", () => {
    expect(stepAgentCat(AGENT_CAT_START, cue("done", "d1", 0), CUE_FRESH_MS).pose).toBe("cheer");
    expect(stepAgentCat(AGENT_CAT_START, cue("done", "d1", 0), CUE_FRESH_MS + 1).pose).toBe("content");
  });

  test("the same one-shot waits exactly REACT_GAP_MS", () => {
    const first = stepAgentCat(AGENT_CAT_START, cue("done", "d1", 1000), 1000);
    const early = stepAgentCat(first, cue("done", "d2", 6999), 6999);
    expect(early.pose).not.toBe("cheer");
    const onTime = stepAgentCat(first, cue("done", "d2", 1000 + REACT_GAP_MS), 1000 + REACT_GAP_MS);
    expect(onTime.pose).toBe("cheer");
  });

  test("an error after a working turn startles again once the gap has passed", () => {
    const e = cue("error", "error:a", 0);
    const s1 = stepAgentCat(AGENT_CAT_START, e, 0);
    const s2 = stepAgentCat(s1, cue("working", "working", 2000), 2000);
    expect(s2.pose).toBe("work");
    const t = REACT_GAP_MS + 100;
    expect(stepAgentCat(s2, cue("error", "error:a", t), t).pose).toBe("startle");
  });

  test("answering an ask returns to the finished session without cheering again", () => {
    const s1 = stepAgentCat(AGENT_CAT_START, cue("done", "done:a:1", 0), 0);
    const s2 = stepAgentCat(s1, cue("ask", "ask:b:1", 100), 100);
    expect(s2.pose).toBe("meow");
    const s3 = stepAgentCat(s2, cue("done", "done:a:1", 0), 3000);
    expect(s3.pose).toBe("content");
  });

  test("a one-shot ends by itself when stepped at its end time", () => {
    const s1 = stepAgentCat(AGENT_CAT_START, cue("ask", "ask:a:1", 0), 0);
    const s2 = stepAgentCat(s1, cue("ask", "ask:a:1", 0), s1.until);
    expect(s2.pose).toBe("alert");
    expect(s2.until).toBe(0);
  });
});
