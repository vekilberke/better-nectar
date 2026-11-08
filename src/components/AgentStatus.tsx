import { useEffect, useState, type ReactNode } from "react";
import { motion } from "framer-motion";
import { useT } from "../i18n";
import { formatStopwatch } from "../clockFormat";
import { setVisibleInterval } from "../hooks/visibleInterval";
import { canOpenAgent, isAgentBusy, type AgentRun } from "../agentSessions";
import { UsageRing, usageTooltip } from "./UsageMeter";
import { usagePercent, type LiveWindow, type UsageState } from "../claudeUsage";

export type AgentState = "working" | "tool" | "needs_input" | "done" | "error";

export interface AgentActivity {
  state: AgentState;
  name?: string;
  project?: string;
  window?: number;
  detail?: string;
  tool?: string;
  session?: string;
  /** Claude desktop app session id (`local_…`) when the desktop app runs the agent. */
  host?: string;
  title?: string;
  idle?: boolean;
  fresh?: boolean;
  ask?: { id: number; tool?: string; detail?: string };
  cancelled?: number;
}

export const MASCOT: Record<AgentState, string> = {
  working: "working",
  tool: "working",
  needs_input: "needs-input",
  done: "done",
  error: "error",
};

export function useAgentLabel(activity: AgentActivity) {
  const t = useT();
  const { state, name, project } = activity;
  const status = t(`agent.${state}`);
  return `${name ?? t("agent.default")} · ${project ? `${status} · ${project}` : status}`;
}

export function AgentMascot({ activity }: { activity: AgentActivity }) {
  return (
    <motion.img
      className="agent-mascot"
      key={activity.state}
      src={`/mascot/${MASCOT[activity.state]}.svg`}
      title={useAgentLabel(activity)}
      alt=""
      draggable={false}
      initial={{ scale: 0.6, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      transition={{ type: "spring", stiffness: 520, damping: 22 }}
    />
  );
}

function AgentElapsed({ run, busy }: { run: AgentRun; busy: boolean }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!busy) return;
    setNow(Date.now());
    const id = setVisibleInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [busy]);

  const until = busy ? now : run.end ?? now;
  return <>{formatStopwatch(Math.max(0, until - run.since), false)}</>;
}

export function agentBarStatus(t: ReturnType<typeof useT>, { state, tool }: AgentActivity) {
  return state === "tool" ? (tool ? `${t("agent.tool")}: ${tool}` : t("agent.tool")) : t(`agent.${state}`);
}

interface AgentBarProps {
  activity: AgentActivity;
  run: AgentRun;
  onClick: () => void;
  /** Claude usage window to show as a ring (only near the limit, see App.tsx). */
  usage?: { window: LiveWindow; all: UsageState } | null;
  /** What replaces the spark (the cat, T-022). */
  icon?: ReactNode;
}

export function AgentBar({ activity, run, onClick, usage, icon }: AgentBarProps) {
  const t = useT();
  const { state } = activity;
  const busy = isAgentBusy(activity);
  const status = agentBarStatus(t, activity);
  const label = useAgentLabel(activity);
  return (
    <motion.div
      key="agent-bar"
      className={`agent-bar ${state}`}
      title={usage ? `${label}
${usageTooltip(t, usage.all, Date.now())}` : label}
      onClick={onClick}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
    >
      {icon ?? <img className="agent-bar-icon" src={state === "working" || state === "tool" ? "/agent-spark.svg" : "/agent-spark-still.svg"} alt="" draggable={false} />}
      <span className="agent-bar-status">{status}</span>
      {run.count > 1 && <span className="agent-bar-count">{run.count}</span>}
      {usage && (
        <span className="agent-bar-usage" aria-label={t("usage.pct", { n: usagePercent(usage.window.used_percentage) })}>
          <UsageRing window={usage.window} size={12} />
        </span>
      )}
      <span className="agent-bar-time"><AgentElapsed run={run} busy={busy} /></span>
    </motion.div>
  );
}

export function AgentDot({ activity }: { activity: AgentActivity }) {
  return <span className={`agent-dot ${activity.state}`} title={useAgentLabel(activity)} />;
}

export function AgentLeft({ activity, dot }: { activity: AgentActivity; dot: boolean }) {
  return (
    <>
      <AgentMascot activity={activity} />
      {dot && <AgentDot activity={activity} />}
    </>
  );
}

export function AgentPanel({ activity, onOpen, onDismiss, onAnswer }: { activity: AgentActivity; onOpen: () => void; onDismiss: () => void; onAnswer: (allow: boolean) => void }) {
  const t = useT();
  const { ask } = activity;
  return (
    <>
      <div className="agent-panel-head">
        <img className="agent-panel-mascot" src={`/mascot/${MASCOT[activity.state]}.svg`} alt="" draggable={false} />
        <div className="agent-panel-text">
          <b>{activity.name ?? t("agent.default")}</b>
          <span>{activity.project ? `${t(`agent.${activity.state}`)} · ${activity.project}` : t(`agent.${activity.state}`)}</span>
        </div>
        <span className={`agent-dot ${activity.state}`} />
      </div>
      {ask && (
        <div className="agent-ask">
          {ask.tool && <b>{ask.tool}</b>}
          {ask.detail && <code>{ask.detail}</code>}
        </div>
      )}
      {!ask && activity.detail && (
        <div className="agent-ask">
          <span className="agent-ask-text">{activity.detail}</span>
        </div>
      )}
      <div className="agent-panel-actions">
        {ask ? (
          <>
            <button className="agent-btn primary" onClick={() => onAnswer(true)}>{t("agent.allow")}</button>
            <button className="agent-btn" onClick={() => onAnswer(false)}>{t("agent.deny")}</button>
          </>
        ) : null}
        <button className={`agent-btn${ask ? "" : " primary"}`} onClick={onOpen} disabled={!canOpenAgent(activity)}>{t("agent.open")}</button>
        {!ask && <button className="agent-btn" onClick={onDismiss}>{t("agent.dismiss")}</button>}
      </div>
    </>
  );
}
