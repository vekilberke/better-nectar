import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { CAT_H, CAT_PALETTES, CAT_W } from "./sonderCatData";
import { Frame, Pupils, useDpr } from "./NotchCat";
import { useAgentLabel } from "./AgentStatus";
import { DEFAULT_CAT_PALETTE } from "../catPalette";
import { AGENT_CAT_START, agentCue, catSpritePx, stepAgentCat, type AgentCatPose, type AgentCatState } from "../catBrain";
import type { AgentSession } from "../agentSessions";

// The cat in the agent mascot's place on the collapsed notch (T-022). Sprites: SondeR cat by Verisonder
// (Apache-2.0), see third_party/sonder-cat; the meow frame and the marks are Nectar's. App plays the sound
// (agentSound in catBrain), so a new permission request always makes one, even while the cat holds a pose.

const HEART = "M1 0h1v1h-1zM3 0h1v1h-1zM0 1h5v1h-5zM1 2h3v1h-3zM2 3h1v1h-1z";
const BANG = "M23 1h2v4h-2zM23 6h2v2h-2z";

// Survives remounts (the bar and the wings swap, hover) so a cheer or a meow never plays twice.
let mood: AgentCatState = AGENT_CAT_START;

function Marks({ pose }: { pose: AgentCatPose }) {
  if (pose === "cheer") {
    return (
      <g fill="#ff6b8a">
        <g transform="translate(-1 4)"><path className="cat-heart" d={HEART} /></g>
        <g transform="translate(21 2)"><path className="cat-heart cat-heart-late" d={HEART} /></g>
      </g>
    );
  }
  if (pose === "meow" || pose === "alert") return <path fill="#ff9f43" d={BANG} />;
  if (pose === "startle") return <path fill="#ff5f5f" d={BANG} />;
  return null;
}

function AgentCatInner({ session, palette = DEFAULT_CAT_PALETTE, active }: { session: AgentSession; palette?: string; active: boolean }) {
  const cue = agentCue(session);
  const cueRef = useRef(cue);
  cueRef.current = cue;
  const [pose, setPose] = useState<AgentCatPose>(mood.pose);
  const [docHidden, setDocHidden] = useState(() => document.hidden);
  const [reduced, setReduced] = useState(() => window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  const dpr = useDpr();
  const pal = CAT_PALETTES[palette] ?? CAT_PALETTES[DEFAULT_CAT_PALETTE];
  const label = useAgentLabel(session.activity);

  useLayoutEffect(() => {
    let timer: number | undefined;
    const step = () => {
      const now = Date.now();
      mood = stepAgentCat(mood, cueRef.current, now);
      setPose(mood.pose);
      if (mood.until > now) timer = window.setTimeout(step, mood.until - now + 20);
    };
    step();
    return () => window.clearTimeout(timer);
  }, [cue.id, cue.mood]);

  useEffect(() => {
    const onVis = () => setDocHidden(document.hidden);
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const onMq = () => setReduced(mq.matches);
    document.addEventListener("visibilitychange", onVis);
    mq.addEventListener("change", onMq);
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      mq.removeEventListener("change", onMq);
    };
  }, []);

  const animated = active && !docHidden && !reduced;

  return (
    <span className={`agent-cat${animated ? "" : " is-still"}`} title={label} role="img" aria-label={label}>
      <svg
        className={`notch-cat-sprite agent-cat-${pose}`}
        width={catSpritePx(CAT_W, dpr)}
        height={catSpritePx(CAT_H, dpr)}
        viewBox={`0 0 ${CAT_W} ${CAT_H}`}
        shapeRendering="crispEdges"
      >
        <g className="cat-hop">
          {pose === "work" && (
            <>
              <Frame name="type_a" pal={pal} className="cat-work-a" />
              <Frame name="type_b" pal={pal} className="cat-work-b" />
              <Pupils frame="type_a" pal={pal} ox={1} oy={1} flip={false} />
            </>
          )}
          {pose === "cheer" && (
            <>
              <Frame name="stretch" pal={pal} className="cat-cheer-up" />
              <Frame name="sit_a" pal={pal} className="cat-cheer-down" />
              <g className="cat-cheer-down"><Pupils frame="sit_a" pal={pal} ox={0} oy={1} flip={false} /></g>
            </>
          )}
          {pose === "content" && <Frame name="blink" pal={pal} />}
          {pose === "meow" && (
            <>
              <Frame name="meow" pal={pal} className="cat-meow-a" />
              <Frame name="sit_a" pal={pal} className="cat-meow-b" />
              <Pupils frame="meow" pal={pal} ox={0} oy={1} flip={false} />
            </>
          )}
          {(pose === "alert" || pose === "startle") && (
            <>
              <Frame name="sit_a" pal={pal} />
              <Pupils frame="sit_a" pal={pal} ox={0} oy={pose === "startle" ? 0 : 1} flip={false} />
            </>
          )}
          {pose === "sad" && (
            <>
              <Frame name="peek" pal={pal} />
              <Pupils frame="peek" pal={pal} ox={0} oy={0} flip={false} />
            </>
          )}
        </g>
        <Marks pose={pose} />
      </svg>
    </span>
  );
}

export const AgentCat = memo(AgentCatInner);
