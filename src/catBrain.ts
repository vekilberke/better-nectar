// Reactions of the idle notch cat to what the user is doing, after SondeR cat by Verisonder
// (Apache-2.0). Rust (cat_input.rs) sends only timing: typing beats, scrolling, away and back.

import { CAT_W } from "./components/sonderCatData";
import { agentGroup, type AgentSession } from "./agentSessions";
import type { AgentActivity } from "./components/AgentStatus";
import type { SoundState } from "./agentSounds";

export type Pose = "sit" | "groom" | "yawn" | "sleep" | "stretch" | "run" | "type" | "play" | "peek";

export type CatInputEvent =
  | { kind: "typing"; beat: number }
  | { kind: "typing-stop" }
  | { kind: "scroll" }
  | { kind: "scroll-stop" }
  | { kind: "away" }
  | { kind: "back"; idleMs: number };

export type Senses = { typing: boolean; scrolling: boolean; near: boolean };

export const QUIET: Senses = { typing: false, scrolling: false, near: false };

export function sense(s: Senses, ev: CatInputEvent): Senses {
  switch (ev.kind) {
    case "typing": return s.typing ? s : { ...s, typing: true };
    case "typing-stop": return s.typing ? { ...s, typing: false } : s;
    case "scroll": return s.scrolling ? s : { ...s, scrolling: true };
    case "scroll-stop": return s.scrolling ? { ...s, scrolling: false } : s;
    default: return s;
  }
}

/** Same priority as SondeR: typing along, then batting at a scrolling page, then hiding from the cursor. */
export function wantedPose(s: Senses): "type" | "play" | "peek" | null {
  if (s.typing) return "type";
  if (s.scrolling) return "play";
  if (s.near) return "peek";
  return null;
}

const REACTIVE = new Set<Pose>(["type", "play", "peek"]);

/**
 * What to do when the senses change: switch to a pose, "resume" the idle wandering, or null (keep going).
 * A stretch or a yawn plays out first and asks again when it ends; a sleeping cat only wakes to type.
 */
export function decide(pose: Pose, s: Senses): Pose | "resume" | null {
  if (pose === "stretch" || pose === "yawn") return null;
  const want = wantedPose(s);
  if (pose === "sleep") return want === "type" ? "type" : null;
  if (want) return want === pose ? null : want;
  return REACTIVE.has(pose) ? "resume" : null;
}

/** Coming back after a break wakes and stretches the cat, unless it is busy typing along. */
export function stretchesOnReturn(pose: Pose): boolean {
  return pose !== "type" && pose !== "stretch";
}

/** Typing paw: alternates with every beat, like SondeR's key counter parity. */
export const pawFrame = (beat: number) => (beat & 1 ? "type_a" : "type_b");

// Roaming. x is the sprite's left edge in CSS px from the middle of the cat's area, so spots stay put
// while the notch springs wider or narrower around its centre.
export type Span = [number, number];

// A walk covers at least twice the cat's width; it stops CAT_GAP px clear of the clock and CAT_EDGE px inside the notch.
export const MIN_WALK = CAT_W * 2;
export const CAT_GAP = 4;
export const CAT_EDGE = 4;
/** CSS px a sprite of px pixels is drawn at: whole device pixels per sprite pixel, so 125% / 150% stay crisp. */
export const catSpritePx = (px: number, dpr: number) => (px * Math.max(1, Math.floor(dpr))) / dpr;
// Room added to the collapsed notch (140 px with the clock) so walks show. Hybrid keeps the clock centred
// with about 70 px free on each side; the cat stops on either side and crosses behind the clock.
export const HYBRID_CAT_EXTRA = 80;
export const CAT_IDLE_EXTRA = 40;

/** Overlapping or touching spans merged, sorted left to right. */
export function mergeSpans(spans: Span[]): Span[] {
  const out: Span[] = [];
  for (const [a, b] of [...spans].filter(([a, b]) => b > a).sort((p, q) => p[0] - q[0])) {
    const last = out[out.length - 1];
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else out.push([a, b]);
  }
  return out;
}

/**
 * Where the cat may stop: inside half (the area's half width) minus edge, and never with any part of
 * the sprite within gap of a blocked span (the clock, music art). Whole pixels.
 */
export function restSpans(half: number, blocked: Span[], catW: number, gap: number, edge: number): Span[] {
  let free: Span[] = [[Math.ceil(-half + edge), Math.floor(half - edge - catW)]];
  for (const [a, b] of mergeSpans(blocked)) {
    const lo = a - gap - catW;
    const hi = b + gap;
    free = free.flatMap(([s, e]): Span[] => {
      if (hi <= s || lo >= e) return [[s, e]];
      const parts: Span[] = [];
      if (lo > s) parts.push([s, Math.floor(lo)]);
      if (hi < e) parts.push([Math.ceil(hi), e]);
      return parts;
    });
  }
  return free.filter(([s, e]) => e >= s);
}

/** The closest spot the cat may stop at, x itself when it already may. */
export function nearestRest(x: number, spans: Span[]): number | null {
  let best: number | null = null;
  for (const [s, e] of spans) {
    const p = Math.min(e, Math.max(s, x));
    if (best === null || Math.abs(p - x) < Math.abs(best - x)) best = p;
  }
  return best;
}

export const canRest = (x: number, spans: Span[]) => spans.some(([s, e]) => x >= s && x <= e);

/**
 * Next walk: a random spot at least minDist away, every reachable pixel equally likely, so most walks
 * cross the clock. A narrow area falls back to its farthest spot if that is at least minDist / 4 away.
 */
export function pickWalk(x: number, spans: Span[], minDist: number, rnd: () => number = Math.random): number | null {
  const parts: Span[] = [];
  for (const [s, e] of spans) {
    const left: Span = [s, Math.min(e, Math.floor(x - minDist))];
    const right: Span = [Math.max(s, Math.ceil(x + minDist)), e];
    if (left[1] >= left[0]) parts.push(left);
    if (right[1] >= right[0]) parts.push(right);
  }
  const total = parts.reduce((n, [s, e]) => n + e - s + 1, 0);
  if (total > 0) {
    let k = Math.min(total - 1, Math.floor(rnd() * total));
    for (const [s, e] of parts) {
      if (k <= e - s) return s + k;
      k -= e - s + 1;
    }
  }
  let far: number | null = null;
  for (const [s, e] of spans) for (const p of [s, e]) if (far === null || Math.abs(p - x) > Math.abs(far - x)) far = p;
  return far !== null && Math.abs(far - x) >= minDist / 4 ? far : null;
}

/** CSS mask that hides the cat behind blocked spans, so a walk passes behind the clock instead of over it. */
export function behindMask(blocked: Span[]): string | undefined {
  const spans = mergeSpans(blocked);
  if (!spans.length) return undefined;
  const at = (v: number) => `calc(50% ${v < 0 ? "-" : "+"} ${Math.abs(Math.round(v))}px)`;
  const stops = spans.flatMap(([a, b]) => [`#000 ${at(a)}`, `transparent ${at(a)}`, `transparent ${at(b)}`, `#000 ${at(b)}`]);
  return `linear-gradient(to right, ${stops.join(", ")})`;
}

// AI agents (T-022): on the collapsed notch the cat takes the agent mascot's place and reacts to the
// session the notch shows. It holds a pose per state; news (a finish, an ask, an error) first plays a short one-shot.
export type AgentMood = "ask" | "waiting" | "error" | "working" | "done";
export type AgentCatPose = "work" | "cheer" | "content" | "meow" | "alert" | "startle" | "sad";

export const CHEER_MS = 2400;
export const MEOW_MS = 1200;
export const STARTLE_MS = 900;
// Only news this fresh plays a one-shot; an older state (the cat appearing after a hover, an idle prompt
// showing a finished session again) just holds its pose.
export const CUE_FRESH_MS = 5000;
// The same one-shot does not start again within this gap, however many sessions change at once.
export const REACT_GAP_MS = 6000;

/** The notch shows one session by priority (agentSessions view: ask > needs input > error > working > done); this is its mood. */
export function agentMood(a: AgentSession["activity"]): AgentMood {
  if (a.ask) return "ask";
  return agentGroup(a);
}

/** What the cat reacts to: id names one piece of news (one ask, one finish), at is when it happened. */
export interface CatCue {
  mood: AgentMood;
  id: string;
  at: number;
}

export function agentCue(s: Pick<AgentSession, "key" | "activity" | "since" | "end" | "seen">): CatCue {
  const mood = agentMood(s.activity);
  switch (mood) {
    case "ask": return { mood, id: `ask:${s.key}:${s.activity.ask!.id}`, at: s.seen };
    case "waiting": return { mood, id: `waiting:${s.key}`, at: s.seen };
    case "error": return { mood, id: `error:${s.key}`, at: s.seen };
    case "done": return { mood, id: `done:${s.key}:${s.end ?? s.since}`, at: s.end ?? s.seen };
    default: return { mood, id: "working", at: s.seen };
  }
}

type OneShot = "cheer" | "meow" | "startle";
const HOLD: Record<AgentMood, AgentCatPose> = { ask: "alert", waiting: "alert", error: "sad", working: "work", done: "content" };
const SHOT: Partial<Record<AgentMood, [OneShot, number]>> = {
  ask: ["meow", MEOW_MS],
  waiting: ["meow", MEOW_MS],
  error: ["startle", STARTLE_MS],
  done: ["cheer", CHEER_MS],
};

export interface AgentCatState {
  cue: string | null;
  pose: AgentCatPose;
  /** End of the one-shot playing, 0 while holding. */
  until: number;
  last: Partial<Record<OneShot, number>>;
}

export const AGENT_CAT_START: AgentCatState = { cue: null, pose: "work", until: 0, last: {} };

/** The cat's next pose for the shown cue. Call it when the cue changes and again at until. */
export function stepAgentCat(s: AgentCatState, cue: CatCue, now: number): AgentCatState {
  const shot = SHOT[cue.mood];
  if (shot && cue.id !== s.cue && now - cue.at <= CUE_FRESH_MS && now - (s.last[shot[0]] ?? -Infinity) >= REACT_GAP_MS) {
    return { cue: cue.id, pose: shot[0], until: now + shot[1], last: { ...s.last, [shot[0]]: now } };
  }
  // A one-shot plays out while the news is of the same kind (another session finishing mid-cheer).
  const playing = !!shot && s.until > now && s.pose === shot[0];
  const pose = playing ? s.pose : HOLD[cue.mood];
  const until = playing ? s.until : 0;
  if (s.cue === cue.id && s.pose === pose && s.until === until) return s;
  return { ...s, cue: cue.id, pose, until };
}

/**
 * The sound an agent update plays: each state and each ask once per session (last: that session's previous
 * key). While the cat is on the notch, an ask or a non-idle wait meows instead of chiming. Only the cat's
 * animation is rate-limited, never the sound, so no permission request goes silent.
 */
export function agentSound(a: Pick<AgentActivity, "state" | "ask" | "idle">, last: string | undefined, catShown: boolean): { key: string; state: SoundState | null; meow: boolean } {
  const key = `${a.state}:${a.ask?.id ?? ""}`;
  const state = key !== last && (a.state === "needs_input" || a.state === "done" || a.state === "error") ? a.state : null;
  return { key, state, meow: state === "needs_input" && catShown && (!!a.ask || !a.idle) };
}
