import { useEffect, useState } from "react";
import { useT } from "../i18n";
import { formatStopwatch } from "../clockFormat";
import { setVisibleInterval } from "../hooks/visibleInterval";
import { agentGroup, canOpenAgent, summarizeAgents, type AgentGroup, type AgentSession } from "../agentSessions";
import { MASCOT, type AgentActivity } from "./AgentStatus";
import { UsageMeter } from "./UsageMeter";
import { primaryWindow, type UsageState } from "../claudeUsage";

const ROW_H = 44;
const ASK_ROW_H = 98;
const ROW_GAP = 3;
const VISIBLE_ROWS = 3;
const HEAD_H = 36;
const CHIPS_H = 24;
const EMPTY_H = 40;

/** Notch panel height for the agents panel, tab strip excluded. */
export function agentsPanelHeight(sessions: readonly AgentSession[], usage = false) {
  // Panel padding 4 + 10, dashed pill 12px * 1.35 + 20 padding + 2 border, chips row 24 + 6 gap.
  if (sessions.length === 0) return HEAD_H + 4 + (usage ? CHIPS_H + 6 : 0) + EMPTY_H + 10;
  const rows = sessions.slice(0, VISIBLE_ROWS).map((s) => (s.activity.ask ? ASK_ROW_H : ROW_H));
  return HEAD_H + 4 + CHIPS_H + 6 + rows.reduce((a, b) => a + b, 0) + (rows.length - 1) * ROW_GAP + 10;
}

const CHIPS: { group: AgentGroup; key: string }[] = [
  { group: "waiting", key: "agent.chip.waiting" },
  { group: "working", key: "agent.chip.working" },
  { group: "error", key: "agent.chip.error" },
  { group: "done", key: "agent.chip.done" },
];

const BADGE: Partial<Record<AgentGroup, string>> = { waiting: "!", done: "✓", error: "×" };

function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = setVisibleInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

function ago(t: (key: string, params?: Record<string, string | number>) => string, ms: number) {
  const mins = Math.floor(ms / 60000);
  if (mins < 1) return t("agent.ago.now");
  if (mins < 60) return t("agent.ago.min", { n: mins });
  return t("agent.ago.hour", { n: Math.floor(mins / 60) });
}

function statusText(t: ReturnType<typeof useT>, a: AgentActivity, end: number | undefined, now: number) {
  switch (a.state) {
    case "tool":
      return [a.tool, a.detail].filter(Boolean).join(" · ") || t("agent.tool");
    case "working":
      return t("agent.working");
    case "needs_input":
      if (a.ask) return t("agent.allowAsk", { tool: a.ask.tool ?? t("agent.default") });
      return a.idle ? t("agent.idle") : a.detail ?? t("agent.needs_input");
    case "done":
      return t("agent.finished", { ago: ago(t, now - (end ?? now)) });
    case "error":
      return a.tool ? `${t("agent.error")} · ${a.tool}` : t("agent.error");
  }
}

interface AgentsListProps {
  sessions: AgentSession[];
  onOpen: (session: AgentSession) => void;
  onAnswer: (session: AgentSession, allow: boolean) => void;
  onClear: () => void;
  /** Claude plan usage for the header meter; null when off or unknown. */
  usage: UsageState | null;
}

export function AgentsList({ sessions, onOpen, onAnswer, onClear, usage }: AgentsListProps) {
  const t = useT();
  const counts = summarizeAgents(sessions);
  const now = useNow(sessions.length > 0);
  const finished = counts.done + counts.error;

  if (sessions.length === 0) {
    return (
      <div className="agents-panel">
        {primaryWindow(usage, Date.now()) && (
          <div className="agents-chips">
            <UsageMeter usage={usage} />
          </div>
        )}
        <div className="clip-empty">{t("agent.none")}</div>
      </div>
    );
  }

  return (
    <div className="agents-panel">
      <div className="agents-chips">
        {CHIPS.filter((c) => counts[c.group] > 0).map((c) => (
          <span key={c.group} className={`agents-chip ${c.group}`}>
            <i />
            {t(c.key, { n: counts[c.group] })}
          </span>
        ))}
        {finished > 0 && (
          <button type="button" className="agents-clear" onClick={onClear}>
            {t("agent.clear")}
          </button>
        )}
        <UsageMeter usage={usage} />
      </div>
      <div className="agents-list">
        {sessions.map((s) => {
          const a = s.activity;
          const group = agentGroup(a);
          const busy = group === "working" || group === "waiting";
          const elapsed = formatStopwatch(Math.max(0, (busy ? now : s.end ?? now) - s.since), false);
          const title = s.title ?? a.name ?? t("agent.default");
          const canOpen = canOpenAgent(a);
          return (
            <div
              key={s.key}
              className={`agents-row ${group}${a.ask ? " asking" : ""}${canOpen ? " clickable" : ""}`}
              onClick={() => canOpen && onOpen(s)}
              title={`${a.name ?? t("agent.default")}${a.project ? ` · ${a.project}` : ""}`}
            >
              <div className="agents-row-main">
                <span className="agents-icon">
                  <img src={`/mascot/${MASCOT[a.state]}.svg`} alt="" draggable={false} />
                  {BADGE[group] && <b className="agents-badge">{BADGE[group]}</b>}
                </span>
                <span className="agents-text">
                  <span className="agents-title">{title}</span>
                  <span className="agents-status">{statusText(t, a, s.end, now)}</span>
                </span>
                <span className="agents-meta">
                  <span className="agents-time">{elapsed}</span>
                  {a.project && <span className="agents-project">{a.project}</span>}
                </span>
              </div>
              {a.ask && (
                <>
                  {a.ask.detail && <code className="agents-ask-detail">{a.ask.detail}</code>}
                  <div className="agents-ask-actions">
                    <button type="button" className="agent-btn primary" onClick={(e) => { e.stopPropagation(); onAnswer(s, true); }}>
                      {t("agent.allow")}
                    </button>
                    <button type="button" className="agent-btn" onClick={(e) => { e.stopPropagation(); onAnswer(s, false); }}>
                      {t("agent.deny")}
                    </button>
                  </div>
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
