// Width of the collapsed agent bar (T-017): sized to icon, status, count and timer, so no gap opens in the middle.
// Keep in step with .agent-bar and its children in App.css.
export const AGENT_BAR_MIN = 140;
export const AGENT_BAR_MAX = 300;
const PAD = 14;
const ICON = 16;
const GAP = 8;
const COUNT_MIN = 16;
const COUNT_PAD = 8;
const RING = 12;
const SLACK = 2;

export interface AgentBarParts {
  status: number;
  time: number;
  count?: number;
  /** The Claude usage ring (T-021) before the timer. */
  meter?: boolean;
  /** Width of what sits in the icon's place (the cat, T-022); the spark icon by default. */
  icon?: number;
}

export function agentBarWidth({ status, time, count, meter, icon = ICON }: AgentBarParts): number {
  const countW = count ? Math.max(COUNT_MIN, count + COUNT_PAD) + GAP : 0;
  const meterW = meter ? RING + GAP : 0;
  const w = PAD * 2 + icon + GAP + status + GAP + countW + meterW + time + SLACK;
  return Math.ceil(Math.min(AGENT_BAR_MAX, Math.max(AGENT_BAR_MIN, w)));
}

/** Widest text the timer shows for a run of this length (digits are measured as zeros). */
export const agentTimerTemplate = (elapsedMs: number) => (elapsedMs >= 3_600_000 ? "0:00:00" : "00:00");

export interface StickyWidth {
  run: number | string;
  width: number;
}

/**
 * What the width sticks to: one session's run, or, while several work at once, all of them together,
 * since the shown session flips between them on almost every hook event.
 */
export const agentBarRunKey = (run: { count: number; since: number }): number | string => (run.count > 1 ? "several" : run.since);

/** Within one run the bar only grows, so tool names of different lengths do not make the notch pulse. */
export function stickyWidth(prev: StickyWidth | null, run: number | string, width: number): StickyWidth {
  if (!prev || prev.run !== run) return { run, width };
  return width > prev.width ? { run, width } : prev;
}
