// Claude plan usage (T-021), from local data only (no polling, no credentials):
// - `line`: the 5-hour and weekly limits Claude Code hands its status line, forwarded by
//   `nectar statusline` (percentage and reset time, terminal sessions only);
// - `desktop`: the newest sample of the Claude desktop app's own usage history (percentage only).

export type UsageWindowKey = "five_hour" | "seven_day";

export interface UsageWindow {
  used_percentage: number;
  /** Unix epoch seconds. */
  resets_at: number;
}

export type ClaudeUsage = Partial<Record<UsageWindowKey, UsageWindow>>;

export interface DesktopSample extends Partial<Record<UsageWindowKey, number>> {
  /** Epoch ms the desktop app took the sample. */
  at: number;
}

export interface UsageState {
  line: ClaudeUsage | null;
  /** Epoch ms of the last status line update. */
  lineAt: number | null;
  desktop: DesktopSample | null;
}

export const USAGE_WINDOWS: UsageWindowKey[] = ["five_hour", "seven_day"];

const WINDOW_MS: Record<UsageWindowKey, number> = { five_hour: 5 * 3_600_000, seven_day: 7 * 24 * 3_600_000 };
/**
 * A desktop sample has no reset time and the app writes it irregularly (hours apart), so past this age
 * it may describe a window that has reset since (T-024).
 */
const DESKTOP_MAX_AGE_MS: Record<UsageWindowKey, number> = { five_hour: 30 * 60_000, seven_day: 6 * 3_600_000 };
/** Both apps read one clock, so a sample from the future was taken while the clock was wrong. */
const DESKTOP_MAX_AHEAD_MS = 5 * 60_000;

const clampPct = (n: number) => Math.min(100, Math.max(0, n));
const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

function windowOf(raw: unknown): UsageWindow | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const { used_percentage: used, resets_at: resets } = raw as Record<string, unknown>;
  if (!isNum(used) || !isNum(resets) || resets <= 0) return undefined;
  return { used_percentage: clampPct(used), resets_at: resets };
}

export function parseUsage(raw: unknown): ClaudeUsage | null {
  if (!raw || typeof raw !== "object") return null;
  const usage: ClaudeUsage = {};
  for (const key of USAGE_WINDOWS) {
    const w = windowOf((raw as Record<string, unknown>)[key]);
    if (w) usage[key] = w;
  }
  return Object.keys(usage).length ? usage : null;
}

function parseDesktop(raw: unknown): DesktopSample | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (!isNum(r.at) || r.at <= 0) return null;
  const sample: DesktopSample = { at: r.at };
  for (const key of USAGE_WINDOWS) {
    const v = r[key];
    if (isNum(v)) sample[key] = clampPct(v);
  }
  return USAGE_WINDOWS.some((k) => sample[k] !== undefined) ? sample : null;
}

/** The `claude_usage` command / `claude-usage` event payload. */
export function parseUsageState(raw: unknown): UsageState {
  const r = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const line = parseUsage(r.status_line);
  const lineAt = line && isNum(r.status_line_at) ? r.status_line_at : null;
  return { line, lineAt, desktop: parseDesktop(r.desktop) };
}

export interface LiveWindow {
  key: UsageWindowKey;
  used_percentage: number;
  /** Milliseconds until the window resets; null when only the desktop app's sample (no reset time) is known. */
  left: number | null;
  /** Epoch ms the percentage was reported. */
  at: number;
  /** The percentage is the desktop app's sample, `at` its age. */
  desktop: boolean;
}

/**
 * Current windows. Usage only grows within a window, so when both sources speak for the same window
 * the higher one wins (a status line can repeat an idle session's old numbers with a fresh time).
 * A status line window whose reset time has passed is gone, and so is a desktop sample taken before
 * that reset, before the live window began, older than `DESKTOP_MAX_AGE_MS` or dated in the future.
 */
export function liveWindows(state: UsageState | null, now: number): LiveWindow[] {
  if (!state) return [];
  return USAGE_WINDOWS.flatMap((key): LiveWindow[] => {
    const line = state.line?.[key];
    const resetAt = line ? line.resets_at * 1000 : 0;
    const live = !!line && resetAt > now;
    const d = state.desktop;
    let dp = d?.[key];
    const stale = !!d && (now - d.at >= DESKTOP_MAX_AGE_MS[key] || d.at - now > DESKTOP_MAX_AHEAD_MS);
    if (d && dp !== undefined && (stale || (line && d.at < (live ? resetAt - WINDOW_MS[key] : resetAt)))) dp = undefined;
    if (line && live) {
      if (d && dp !== undefined && dp > line.used_percentage) return [{ key, used_percentage: dp, left: resetAt - now, at: d.at, desktop: true }];
      return [{ key, used_percentage: line.used_percentage, left: resetAt - now, at: state.lineAt ?? 0, desktop: false }];
    }
    if (d && dp !== undefined) return [{ key, used_percentage: dp, left: null, at: d.at, desktop: true }];
    return [];
  });
}

/** The window the meter shows: the one closest to its limit (the 5-hour one on a tie). */
export function primaryWindow(state: UsageState | null, now: number): LiveWindow | null {
  return liveWindows(state, now).reduce<LiveWindow | null>((best, w) => (!best || w.used_percentage > best.used_percentage ? w : best), null);
}

export type UsageLevel = "ok" | "warn" | "high";

export const usageLevel = (percent: number): UsageLevel => (percent >= 90 ? "high" : percent >= 75 ? "warn" : "ok");

/** Shown percentage: whole numbers, but never 100 before the limit is really hit. */
export const usagePercent = (percent: number) => (percent >= 100 ? 100 : Math.min(99, Math.round(percent)));

export type ResetIn = { unit: "min"; m: number } | { unit: "hour"; h: number; m: number } | { unit: "day"; d: number; h: number };

/** Time to a reset in the two largest units, rounded up to the minute. */
export function resetIn(ms: number): ResetIn {
  const mins = Math.max(1, Math.ceil(ms / 60_000));
  if (mins < 60) return { unit: "min", m: mins };
  if (mins < 24 * 60) return { unit: "hour", h: Math.floor(mins / 60), m: mins % 60 };
  return { unit: "day", d: Math.floor(mins / (24 * 60)), h: Math.floor((mins % (24 * 60)) / 60) };
}

export function sameUsageState(a: UsageState, b: UsageState) {
  return (
    a.lineAt === b.lineAt &&
    a.desktop?.at === b.desktop?.at &&
    USAGE_WINDOWS.every(
      (k) => a.line?.[k]?.used_percentage === b.line?.[k]?.used_percentage && a.line?.[k]?.resets_at === b.line?.[k]?.resets_at && a.desktop?.[k] === b.desktop?.[k]
    )
  );
}

export const EMPTY_USAGE: UsageState = { line: null, lineAt: null, desktop: null };
