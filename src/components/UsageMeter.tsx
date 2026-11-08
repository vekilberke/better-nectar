import { useEffect, useState } from "react";
import { timeAgo, useT } from "../i18n";
import { setVisibleInterval } from "../hooks/visibleInterval";
import { liveWindows, primaryWindow, resetIn, usageLevel, usagePercent, type LiveWindow, type UsageState } from "../claudeUsage";

type T = ReturnType<typeof useT>;

export function resetText(t: T, ms: number) {
  const r = resetIn(ms);
  if (r.unit === "min") return t("usage.left.min", { m: r.m });
  if (r.unit === "hour") return t("usage.left.hour", { h: r.h, m: r.m });
  return t("usage.left.day", { d: r.d, h: r.h });
}

/** "in 2h 13m" when the reset time is known, else how old the desktop app's sample is. */
const whenText = (t: T, w: LiveWindow) => (w.left !== null ? resetText(t, w.left) : timeAgo(w.at, t));

export function usageTooltip(t: T, usage: UsageState | null, now: number) {
  const lines = liveWindows(usage, now).map((w) => {
    const params = { window: t(`usage.window.${w.key}`), pct: usagePercent(w.used_percentage) };
    if (w.left === null) return t("usage.lineAsOf", { ...params, ago: timeAgo(w.at, t) });
    const line = t("usage.line", { ...params, left: resetText(t, w.left) });
    return w.desktop ? `${line} · ${t("usage.fromApp", { ago: timeAgo(w.at, t) })}` : line;
  });
  return [t("usage.title"), ...lines].join("\n");
}

/** Re-renders every 30 s while shown, for the time-to-reset text (stops with the notch hidden). */
function useMinuteNow() {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const id = setVisibleInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(id);
  }, []);
  return now;
}

export function UsageRing({ window, size }: { window: LiveWindow; size: number }) {
  const stroke = size <= 12 ? 2 : 2.5;
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const filled = (Math.min(100, window.used_percentage) / 100) * c;
  return (
    <svg className={`usage-ring ${usageLevel(window.used_percentage)}`} width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
      <circle className="usage-ring-track" cx={size / 2} cy={size / 2} r={r} strokeWidth={stroke} fill="none" />
      <circle
        className="usage-ring-fill"
        cx={size / 2}
        cy={size / 2}
        r={r}
        strokeWidth={stroke}
        fill="none"
        strokeLinecap="round"
        strokeDasharray={`${filled} ${c}`}
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
      />
    </svg>
  );
}

/** Agents panel header: ring, percentage and time to reset of the window closest to its limit; all windows in the tooltip. */
export function UsageMeter({ usage }: { usage: UsageState | null }) {
  const t = useT();
  const now = useMinuteNow();
  const main = primaryWindow(usage, now);
  if (!main) return null;
  const tooltip = usageTooltip(t, usage, now);
  return (
    <span className={`usage-meter ${usageLevel(main.used_percentage)}`} title={tooltip} role="img" aria-label={tooltip}>
      <UsageRing window={main} size={14} />
      <span className="usage-window">{t(`usage.short.${main.key}`)}</span>
      <b className="usage-pct">{t("usage.pct", { n: usagePercent(main.used_percentage) })}</b>
      <span className="usage-reset">{whenText(t, main)}</span>
    </span>
  );
}
