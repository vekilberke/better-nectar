import type { AgentActivity } from "./components/AgentStatus";

export interface AgentRun {
  count: number;
  since: number;
  end?: number;
}

export interface AgentSession {
  key: string;
  activity: AgentActivity;
  /** First user prompt of the session, shortened. Memory only, never persisted. */
  title?: string;
  since: number;
  end?: number;
  seen: number;
  /** Hidden from the notch status (dismissed); still listed in the agents panel. */
  hidden?: boolean;
}

export type AgentGroup = "waiting" | "working" | "done" | "error";

export interface AgentView {
  focus: AgentSession | null;
  count: number;
  run: AgentRun;
}

export interface AgentSource {
  session?: string;
  window?: number;
  name?: string;
  project?: string;
}

/** Rust serializes empty Option fields as JSON null; the store works with missing fields only. */
export function dropNulls<T extends object>(value: T): T {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(value)) {
    if (v === null || v === undefined) continue;
    out[k] = k === "ask" && typeof v === "object" ? dropNulls(v as object) : v;
  }
  return out as T;
}

export const STALE_MS = 2 * 60 * 60 * 1000;
export const ERROR_GRACE_MS = 60 * 1000;
export const DONE_SHOW_MS = 8 * 1000;

export const isAgentBusy = (a: { state: AgentActivity["state"]; idle?: boolean }) =>
  a.state === "working" || a.state === "tool" || (a.state === "needs_input" && !a.idle);

/** A click can take the user there: the terminal window, or the session in the Claude desktop app. */
export const canOpenAgent = (a: Pick<AgentActivity, "window" | "session" | "host">) => a.window !== undefined || !!a.session || !!a.host;

export function agentGroup(a: { state: AgentActivity["state"]; idle?: boolean }): AgentGroup {
  if (a.state === "error") return "error";
  if (a.state === "needs_input" && !a.idle) return "waiting";
  if (a.state === "working" || a.state === "tool") return "working";
  return "done";
}

export const agentSessionKey = (a: AgentSource) =>
  a.session ?? String(a.window ?? `${a.name ?? ""}:${a.project ?? ""}`);

/** Manual `nectar agent` calls have no session id; match them to a hook session by window, else by name and project. */
function sameSource(a: AgentSource, b: AgentSource) {
  if (a.window !== undefined && b.window !== undefined) return a.window === b.window;
  return (a.name ?? "") === (b.name ?? "") && (a.project ?? "") === (b.project ?? "");
}

/** Time until which a session counts for the notch status display. */
function shownUntil(s: AgentSession) {
  const group = agentGroup(s.activity);
  if (group === "waiting" || group === "working") return s.seen + STALE_MS;
  return s.seen + (group === "error" ? ERROR_GRACE_MS : DONE_SHOW_MS);
}

const GROUP_ORDER: Record<AgentGroup, number> = { waiting: 0, working: 1, error: 2, done: 3 };

function focusRank(s: AgentSession) {
  if (s.activity.ask) return 0;
  const group = agentGroup(s.activity);
  return group === "waiting" ? 1 : group === "error" ? 2 : group === "working" ? 3 : 4;
}

export function summarizeAgents(sessions: readonly AgentSession[]): Record<AgentGroup, number> {
  const counts: Record<AgentGroup, number> = { waiting: 0, working: 0, done: 0, error: 0 };
  for (const s of sessions) counts[agentGroup(s.activity)]++;
  return counts;
}

export function createAgentSessions() {
  const sessions = new Map<string, AgentSession>();

  const prune = (now: number) => {
    sessions.forEach((s, k) => {
      if (now - s.seen > STALE_MS) sessions.delete(k);
    });
  };

  const shown = (now: number) => [...sessions.values()].filter((s) => !s.hidden && now < shownUntil(s));

  return {
    update(raw: AgentActivity, now = Date.now()): AgentSession {
      const activity = dropNulls(raw);
      const key = agentSessionKey(activity);
      const prev = sessions.get(key);
      // A pending permission request holds the row until it is answered or cancelled.
      if (prev?.activity.ask && !activity.ask && activity.cancelled !== prev.activity.ask.id && (activity.state === "working" || activity.state === "tool")) {
        return prev;
      }
      const busy = isAgentBusy(activity);
      const resumed = busy && !activity.fresh && prev && (isAgentBusy(prev.activity) || (prev.activity.state === "error" && now - prev.seen < ERROR_GRACE_MS));
      const merged: AgentActivity = {
        ...activity,
        name: activity.name ?? prev?.activity.name,
        project: activity.project ?? prev?.activity.project,
        window: activity.window ?? prev?.activity.window,
        host: activity.host ?? prev?.activity.host,
      };
      const session: AgentSession = {
        key,
        activity: merged,
        title: prev?.title ?? activity.title,
        since: resumed || (!busy && prev) ? prev!.since : now,
        // A later `done` always sets the finish time, even after an error ended the run early.
        end: busy ? undefined : activity.state === "done" ? now : prev?.end ?? now,
        seen: now,
      };
      sessions.set(key, session);
      prune(now);
      return session;
    },

    /** Removes the ended session, plus manual (id-less) sessions from the same source. Returns how many were removed. */
    end(raw: AgentSource): number {
      const source = dropNulls(raw);
      const key = agentSessionKey(source);
      let removed = sessions.delete(key) ? 1 : 0;
      sessions.forEach((s, k) => {
        const hasId = s.activity.session !== undefined;
        if ((source.session === undefined || !hasId) && sameSource(s.activity, source)) {
          sessions.delete(k);
          removed++;
        }
      });
      return removed;
    },

    answer(key: string, now = Date.now()) {
      const s = sessions.get(key);
      if (s) sessions.set(key, { ...s, activity: { ...s.activity, state: "working", ask: undefined }, end: undefined, seen: now });
    },

    dismiss(key: string) {
      const s = sessions.get(key);
      if (s) sessions.set(key, { ...s, hidden: true });
    },

    /** Drops finished and failed sessions from the list. */
    clearFinished(): number {
      let removed = 0;
      sessions.forEach((s, k) => {
        const group = agentGroup(s.activity);
        if (group === "done" || group === "error") {
          sessions.delete(k);
          removed++;
        }
      });
      return removed;
    },

    /** Every session for the agents panel: waiting first, then working, failed, done; newest run first within a group. */
    list(now = Date.now()): AgentSession[] {
      prune(now);
      return [...sessions.values()].sort((a, b) => GROUP_ORDER[agentGroup(a.activity)] - GROUP_ORDER[agentGroup(b.activity)] || b.since - a.since);
    },

    /** What the collapsed notch shows: one session by priority plus how many are active. */
    view(now = Date.now()): AgentView {
      prune(now);
      let focus: AgentSession | null = null;
      let count = 0;
      for (const s of shown(now)) {
        const group = agentGroup(s.activity);
        if (group === "waiting" || group === "working" || group === "error") count++;
        if (!focus || focusRank(s) < focusRank(focus) || (focusRank(s) === focusRank(focus) && s.seen > focus.seen)) focus = s;
      }
      return { focus, count, run: { count, since: focus?.since ?? now, end: focus?.end } };
    },

    /** When the status display next needs to re-evaluate (a done or failed session expiring). */
    nextExpiry(now = Date.now()): number | null {
      let next: number | null = null;
      for (const s of sessions.values()) {
        if (s.hidden) continue;
        const at = shownUntil(s);
        if (at > now && (next === null || at < next)) next = at;
      }
      return next;
    },

    reset() {
      sessions.clear();
    },
  };
}
