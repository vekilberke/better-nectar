import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../i18n";
import { CAT_EYES, CAT_FRAMES, CAT_H, CAT_PALETTES, CAT_W } from "./sonderCatData";

import { DEFAULT_CAT_PALETTE } from "../catPalette";
import {
  behindMask, CAT_EDGE, CAT_GAP, decide, MIN_WALK, nearestRest, pawFrame, pickWalk, QUIET, restSpans, sense,
  stretchesOnReturn, type CatInputEvent, type Pose, type Senses, type Span,
} from "../catBrain";

// Sprites and reactions: SondeR cat by Verisonder (Apache-2.0), see third_party/sonder-cat.

const SPEED = 36;
const Z_PATH = "M0 0h3v1h-3zM1 1h1v1h-1zM0 2h3v1h-3z";
// Cursor this close to the cat (CSS px) makes it duck and peek, usually just before the notch opens.
const NEAR_X = 44;
const NEAR_Y = 40;

type Brain = { x: number; flip: boolean; pose: Pose; awakeSince: number; sleepUntil: number; lastActive: number; deep: boolean };

// Survives unmounts (hover, timers) so the cat keeps its mood, sleep and spot.
// x: sprite's left edge from the middle of the area; it starts left of the clock and the first step checks it.
// deep: the user is away, the cat sleeps until they are back.
const brain: Brain = { x: -(CAT_W * 2 + 4), flip: false, pose: "sit", awakeSince: Date.now(), sleepUntil: 0, lastActive: 0, deep: false };

// One input watch per window, also while an exiting cat and its replacement overlap.
let watching = 0;
const watchInput = (on: boolean) => {
  watching += on ? 1 : -1;
  if (on ? watching === 1 : watching === 0) invoke("set_cat_input_watch", { enabled: on }).catch(() => {});
};

const rand = (min: number, max: number) => min + Math.random() * (max - min);
const isNight = () => { const h = new Date().getHours(); return h >= 23 || h < 6; };
const TRANSIENT = new Set<Pose>(["run", "type", "play", "peek"]);

const LETTERS = ["O", "K", "B", "S", "W", "E", "Z", "N", "M", "H", "G", "g"];

export function Frame({ name, pal, className }: { name: string; pal: Record<string, string>; className?: string }) {
  const layers = CAT_FRAMES[name];
  return (
    <g className={className}>
      {LETTERS.map((c) => layers[c] ? <path key={c} fill={pal[c] ?? "#ff00ff"} d={layers[c]} /> : null)}
    </g>
  );
}

export function Pupils({ frame, pal, ox, oy, flip, blink }: { frame: string; pal: Record<string, string>; ox: number; oy: number; flip: boolean; blink?: boolean }) {
  const eyes = CAT_EYES[frame];
  if (!eyes) return null;
  const px = flip ? 1 - ox : ox;
  return (
    <g className={blink ? "cat-pupils" : undefined} fill={pal.P}>
      {eyes.map(([ex, ey]) => <rect key={ex} x={ex + px} y={ey + oy} width="2" height="2" />)}
    </g>
  );
}

export function useDpr() {
  const [dpr, setDpr] = useState(() => window.devicePixelRatio || 1);
  useEffect(() => {
    let mq: MediaQueryList;
    const onChange = () => { setDpr(window.devicePixelRatio || 1); bind(); };
    const bind = () => {
      mq?.removeEventListener("change", onChange);
      mq = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      mq.addEventListener("change", onChange);
    };
    bind();
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return dpr;
}

/**
 * The cat roams the whole width of its parent. avoid: selector, looked up inside the closest
 * [data-cat-area], for things it never stops on and walks behind (the clock, music art).
 */
function NotchCatInner({ active, palette = DEFAULT_CAT_PALETTE, avoid, layoutKey }: { active: boolean; palette?: string; avoid?: string; layoutKey?: string }) {
  const t = useT();
  const [pose, setPose] = useState<Pose>(TRANSIENT.has(brain.pose) ? "sit" : brain.pose);
  // Deep sleep while the user is away: CSS loops pause so an unattended notch costs nothing.
  const [deepSleep, setDeepSleep] = useState(brain.deep);
  const [x, setX] = useState(brain.x);
  const [mask, setMask] = useState<string | undefined>(undefined);
  const areaRef = useRef<HTMLDivElement>(null);
  const avoidRef = useRef(avoid);
  avoidRef.current = avoid;
  const maskRef = useRef<string | undefined>(undefined);
  const [flip, setFlip] = useState(brain.flip);
  const [duration, setDuration] = useState(0);
  const [gaze, setGaze] = useState<[number, number]>([0, 1]);
  const [beat, setBeat] = useState(0);
  const [docHidden, setDocHidden] = useState(() => document.hidden);
  const [reduced, setReduced] = useState(() => window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  const [looking, setLooking] = useState(false);
  const walkerRef = useRef<HTMLDivElement>(null);
  const xRef = useRef(x);
  const poseRef = useRef(pose);
  const lookingRef = useRef(false);
  const nearRef = useRef<((near: boolean) => void) | null>(null);
  const dpr = useDpr();
  // Whole device pixels per sprite pixel, so the pixel art stays crisp at 125% / 150% scaling.
  const k = Math.max(1, Math.floor(dpr));
  const running = active && !docHidden;
  const pal = CAT_PALETTES[palette] ?? CAT_PALETTES[DEFAULT_CAT_PALETTE];

  const commitPose = (p: Pose) => { poseRef.current = p; brain.pose = p; setPose(p); };

  const placeAt = (v: number) => { xRef.current = v; brain.x = v; setX(v); };

  const walkerX = () => walkerRef.current ? Math.round(new DOMMatrixReadOnly(getComputedStyle(walkerRef.current).transform).m41) : xRef.current;

  // Read only when the cat decides something (every few seconds), so nothing watches the layout in between.
  const measure = (): Span[] | null => {
    const el = areaRef.current;
    if (!el || !el.offsetWidth) return null;
    const r = el.getBoundingClientRect();
    const z = r.width / el.offsetWidth || 1;
    const cx = r.left + r.width / 2;
    const blocked: Span[] = [];
    const sel = avoidRef.current;
    const scope = sel ? el.closest("[data-cat-area]") : null;
    scope?.querySelectorAll(sel!).forEach((b) => {
      const br = b.getBoundingClientRect();
      if (br.width > 0 && br.height > 0) blocked.push([(br.left - cx) / z - 1, (br.right - cx) / z + 1]);
    });
    const m = behindMask(blocked);
    if (m !== maskRef.current) { maskRef.current = m; setMask(m); }
    return restSpans(el.offsetWidth / 2, blocked, CAT_W, CAT_GAP, CAT_EDGE);
  };

  // Clamp to a free spot without walking; a walk in progress is checked again when it arrives.
  const settleSpot = () => {
    const rest = measure();
    if (!rest || poseRef.current === "run") return;
    const spot = nearestRest(xRef.current, rest);
    if (spot !== null && spot !== xRef.current) placeAt(spot);
  };
  const settleRef = useRef(settleSpot);
  settleRef.current = settleSpot;

  useLayoutEffect(() => { settleSpot(); }, []);

  // The area or what it avoids changed (mode switch, dual clock, timer, music): re-read even while the cat naps.
  const relayoutRef = useRef<number | undefined>(undefined);
  const relayout = () => {
    window.clearTimeout(relayoutRef.current);
    relayoutRef.current = window.setTimeout(() => settleRef.current(), 250);
  };
  useEffect(() => {
    const el = areaRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(relayout);
    ro.observe(el);
    return () => { ro.disconnect(); window.clearTimeout(relayoutRef.current); };
  }, []);
  useEffect(() => { relayout(); }, [layoutKey]);

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

  // Typing, scrolling and presence come from the Rust hooks, only while this cat is on screen and moving.
  useEffect(() => {
    if (!running || reduced) return;
    watchInput(true);
    return () => watchInput(false);
  }, [running, reduced]);

  useEffect(() => {
    if (!running || reduced) {
      if (poseRef.current === "run") placeAt(walkerX());
      setDuration(0);
      // Stopped mid-walk behind the clock: stand on the nearest free spot instead.
      const rest = measure();
      const spot = rest ? nearestRest(xRef.current, rest) : null;
      if (spot !== null && spot !== xRef.current) placeAt(spot);
      if (reduced) { brain.sleepUntil = 0; brain.deep = false; setDeepSleep(false); brain.awakeSince = Date.now(); }
      if (reduced || poseRef.current !== "sleep") commitPose("sit");
      return;
    }
    // The input watch restarts with the cat, so a deep sleep from before cannot get its "back": wake up now.
    if (brain.deep) { brain.deep = false; setDeepSleep(false); brain.sleepUntil = Date.now(); }
    // Time with no cat on screen is not awake time: restart the clock after a real absence.
    if (Date.now() - brain.lastActive > 5000 && !brain.sleepUntil) brain.awakeSince = Date.now();
    let timer: number | undefined;
    let senses: Senses = QUIET;
    // The first idle step after appearing, waking or the user stopping typing is a walk, so roaming shows right away.
    let walkNext = true;
    const wait = (ms: number, fn: () => void) => { window.clearTimeout(timer); timer = window.setTimeout(fn, ms); };

    const glance = () => {
      if (!lookingRef.current) setGaze([Math.random() < 0.5 ? 0 : 1, Math.random() < 0.7 ? 1 : 0]);
    };

    const walkTo = (target: number, done: () => void) => {
      const dist = Math.abs(target - xRef.current);
      const ms = (dist / SPEED) * 1000;
      const right = target > xRef.current;
      brain.flip = right;
      setFlip(right);
      setDuration(ms);
      commitPose("run");
      placeAt(target);
      wait(ms + 40, () => { setDuration(0); done(); });
    };

    const arrive = () => { commitPose("sit"); glance(); wait(rand(1200, 3000), step); };

    // Stop wherever the walk is, so a reaction starts on the spot; behind the clock it first steps out.
    const halt = (then: () => void) => {
      window.clearTimeout(timer);
      if (poseRef.current !== "run" || !walkerRef.current) { then(); return; }
      placeAt(walkerX());
      setDuration(0);
      const rest = measure();
      const spot = rest ? nearestRest(xRef.current, rest) : null;
      if (spot === null || spot === xRef.current) then();
      else walkTo(spot, then);
    };

    const awaken = () => { brain.sleepUntil = 0; brain.deep = false; setDeepSleep(false); brain.awakeSince = Date.now(); walkNext = true; };

    const react = () => {
      const next = decide(poseRef.current, senses);
      if (next === "resume") {
        // Typing along or batting at the page is awake time spent with the user.
        if (poseRef.current === "type" || poseRef.current === "play") { brain.awakeSince = Date.now(); walkNext = true; }
        commitPose("sit");
        wait(rand(1200, 2500), step);
      } else if (next) {
        if (poseRef.current === "sleep") { awaken(); commitPose(next); return; }
        // Asked again on arrival: the user may have stopped while the cat stepped out from behind the clock.
        halt(() => {
          const now = decide("sit", senses);
          if (now && now !== "resume") commitPose(now);
          else { commitPose("sit"); wait(rand(1200, 2500), step); }
        });
      }
    };

    // After a stretch: type along if the keys are still going, otherwise sit and wander again.
    const settle = () => {
      const next = decide("sit", senses);
      if (next && next !== "resume") { commitPose(next); return; }
      commitPose("sit");
      wait(rand(1500, 3000), step);
    };

    const wake = () => {
      awaken();
      commitPose("stretch");
      wait(1500, () => { brain.awakeSince = Date.now(); settle(); });
    };

    const step = () => {
      const now = Date.now();
      // Typing that began during the yawn would not reach react() again, so check before napping.
      if ((brain.deep || brain.sleepUntil > now) && decide("sleep", senses) === "type") {
        awaken();
        commitPose("type");
        return;
      }
      if (brain.deep) { commitPose("sleep"); return; }
      if (brain.sleepUntil > now) {
        commitPose("sleep");
        wait(brain.sleepUntil - now, step);
        return;
      }
      if (brain.sleepUntil) { wake(); return; }
      const want = decide("sit", senses);
      if (want && want !== "resume") { commitPose(want); return; }
      const awakeLimit = isNight() ? rand(20000, 40000) : rand(70000, 130000);
      if (now - brain.awakeSince > awakeLimit) {
        commitPose("yawn");
        wait(1800, () => {
          brain.sleepUntil = Date.now() + rand(60000, 150000);
          step();
        });
        return;
      }
      // The layout changed under the cat (dual clock, music art, a narrower notch): move clear first.
      const rest = measure();
      const spot = rest ? nearestRest(xRef.current, rest) : null;
      if (spot !== null && Math.abs(spot - xRef.current) > 2) { walkTo(spot, arrive); return; }
      // A cursor nearby does not stop the walks: the eyes follow it on the way, a close cursor makes it peek.
      const roll = walkNext ? 0 : Math.random();
      walkNext = false;
      if (roll < 0.45) {
        const target = rest ? pickWalk(xRef.current, rest, MIN_WALK) : null;
        if (target !== null) { walkTo(target, arrive); return; }
      }
      if (roll < 0.65) {
        commitPose("groom");
        wait(rand(2400, 4200), () => { commitPose("sit"); wait(rand(1500, 3500), step); });
        return;
      }
      // Hides behind the notch edge for a moment and peeks out.
      if (roll < 0.72) {
        commitPose("peek");
        wait(rand(2500, 5000), () => { commitPose("sit"); glance(); wait(rand(1500, 3000), step); });
        return;
      }
      commitPose("sit");
      glance();
      wait(rand(2500, 6000), step);
    };

    const onInput = (ev: CatInputEvent) => {
      if (ev.kind === "typing") setBeat(ev.beat);
      if (ev.kind === "away") {
        brain.deep = true;
        setDeepSleep(true);
        const p = poseRef.current;
        if (p === "sit" || p === "groom" || p === "run" || p === "peek") {
          halt(() => { commitPose("yawn"); wait(1800, step); });
        }
        return;
      }
      if (ev.kind === "back") {
        if (stretchesOnReturn(poseRef.current)) halt(wake);
        else awaken();
        return;
      }
      const next = sense(senses, ev);
      if (next === senses) return;
      senses = next;
      react();
    };

    // A short duck when the cursor arrives; a cursor resting by the cat does not keep it hidden.
    let peekTimer: number | undefined;
    let nearArmed = true;
    const unpeek = () => {
      window.clearTimeout(peekTimer);
      if (!senses.near) return;
      senses = { ...senses, near: false };
      react();
    };
    nearRef.current = (near) => {
      if (!near) { nearArmed = true; unpeek(); return; }
      if (!nearArmed) return;
      nearArmed = false;
      senses = { ...senses, near: true };
      react();
      peekTimer = window.setTimeout(unpeek, rand(2000, 4000));
    };

    let unlisten: (() => void) | undefined;
    let gone = false;
    listen<CatInputEvent>("cat-input", (e) => onInput(e.payload))
      .then((u) => { if (gone) u(); else unlisten = u; })
      .catch(() => {});

    // Resume mid-state: a pose left over from a pause is finished first.
    wait(rand(400, 1200), step);
    return () => {
      gone = true;
      unlisten?.();
      nearRef.current = null;
      window.clearTimeout(timer);
      window.clearTimeout(peekTimer);
      brain.lastActive = Date.now();
    };
  }, [running, reduced]);

  useEffect(() => {
    if (!running || reduced) return;
    let raf = 0;
    let idle: number | undefined;
    let last: { x: number; y: number } | null = null;

    const apply = () => {
      raf = 0;
      const pt = last;
      const el = walkerRef.current;
      if (!pt || !el) return;
      const r = el.getBoundingClientRect();
      const dx = pt.x - (r.left + r.width / 2);
      const dy = pt.y - (r.top + r.height * 0.3);
      const ox = dx > 4 ? 1 : 0;
      const oy = dy > -4 ? 1 : 0;
      setGaze((g) => (g[0] === ox && g[1] === oy ? g : [ox, oy]));
      nearRef.current?.(Math.abs(dx) < NEAR_X && Math.abs(dy) < NEAR_Y);
      // Turn to face the cursor unless running or asleep.
      const p = poseRef.current;
      if (p !== "run" && p !== "sleep" && Math.abs(dx) > 6 && brain.flip !== dx > 0) {
        brain.flip = dx > 0;
        setFlip(brain.flip);
      }
    };
    const setLook = (v: boolean) => { lookingRef.current = v; setLooking(v); };
    const onPoint = (x: number, y: number) => {
      last = { x, y };
      if (!lookingRef.current) setLook(true);
      window.clearTimeout(idle);
      idle = window.setTimeout(() => setLook(false), 4000);
      if (!raf) raf = requestAnimationFrame(apply);
    };
    const onMove = (ev: MouseEvent) => onPoint(ev.clientX, ev.clientY);
    const onLeave = () => { window.clearTimeout(idle); setLook(false); nearRef.current?.(false); };

    // The notch window is click-through away from the notch, so the Rust hit-test loop reports the cursor nearby.
    let unlisten: (() => void) | undefined;
    let gone = false;
    listen<[number, number] | null>("notch-cursor", (e) => {
      if (e.payload) onPoint(e.payload[0], e.payload[1]);
      else onLeave();
    }).then((u) => { if (gone) u(); else unlisten = u; }).catch(() => {});

    window.addEventListener("mousemove", onMove, { passive: true });
    document.documentElement.addEventListener("mouseleave", onLeave);
    return () => {
      gone = true;
      unlisten?.();
      window.removeEventListener("mousemove", onMove);
      document.documentElement.removeEventListener("mouseleave", onLeave);
      if (raf) cancelAnimationFrame(raf);
      window.clearTimeout(idle);
      lookingRef.current = false;
      setLooking(false);
    };
  }, [running, reduced]);

  const animated = running && !reduced && !(deepSleep && pose === "sleep");

  return (
    <div
      ref={areaRef}
      className={`notch-cat${animated ? "" : " is-still"}${looking ? " is-looking" : ""}`}
      style={{ height: CAT_H + 4, maskImage: mask, WebkitMaskImage: mask }}
      role="img"
      aria-label={t("notch.idle.catLabel")}
    >
      <div
        ref={walkerRef}
        className="notch-cat-walker"
        style={{ transform: `translateX(${Math.round(x * dpr) / dpr}px)`, transitionDuration: `${duration}ms` }}
      >
        <svg
          className={`notch-cat-sprite pose-${pose}${flip ? " face-flip" : ""}`}
          width={(CAT_W * k) / dpr}
          height={(CAT_H * k) / dpr}
          viewBox={`0 0 ${CAT_W} ${CAT_H}`}
          shapeRendering="crispEdges"
        >
          {pose === "sit" && (
            <>
              <Frame name="sit_a" pal={pal} className="cat-frame cat-wag-a" />
              <Frame name="sit_b" pal={pal} className="cat-frame cat-wag-b" />
              <Frame name="blink" pal={pal} className="cat-blink" />
              <Pupils frame="sit_a" pal={pal} ox={gaze[0]} oy={gaze[1]} flip={flip} blink />
            </>
          )}
          {pose === "run" && (
            <>
              <Frame name="run_a" pal={pal} className="cat-frame cat-run-a" />
              <Frame name="run_b" pal={pal} className="cat-frame cat-run-b" />
              <Pupils frame="run_a" pal={pal} ox={gaze[0]} oy={gaze[1]} flip={flip} />
            </>
          )}
          {pose === "type" && (
            <>
              <Frame name={pawFrame(beat)} pal={pal} />
              <Pupils frame="type_a" pal={pal} ox={gaze[0]} oy={gaze[1]} flip={flip} />
            </>
          )}
          {pose === "play" && (
            <>
              <Frame name="knead_c" pal={pal} className="cat-frame cat-play-c" />
              <Frame name="knead_b" pal={pal} className="cat-frame cat-play-b" />
              <Frame name="knead_a" pal={pal} className="cat-frame cat-play-a" />
              <Pupils frame="knead_a" pal={pal} ox={gaze[0]} oy={gaze[1]} flip={flip} />
            </>
          )}
          {pose === "peek" && (
            <>
              <Frame name="peek" pal={pal} />
              <Pupils frame="peek" pal={pal} ox={gaze[0]} oy={gaze[1]} flip={flip} />
            </>
          )}
          {pose === "groom" && (
            <>
              <Frame name="groom_a" pal={pal} className="cat-frame cat-groom-a" />
              <Frame name="groom_b" pal={pal} className="cat-frame cat-groom-b" />
            </>
          )}
          {pose === "yawn" && <Frame name="yawn" pal={pal} />}
          {pose === "stretch" && <Frame name="stretch" pal={pal} />}
          {pose === "sleep" && (
            <>
              <Frame name="sleep" pal={pal} className="cat-breathe" />
              <g fill="#ffffff">
                <g transform="translate(18 3)"><path className="cat-z1" d={Z_PATH} /></g>
                <g transform="translate(22 0)"><path className="cat-z2" d={Z_PATH} /></g>
              </g>
            </>
          )}
        </svg>
      </div>
    </div>
  );
}

export const NotchCat = memo(NotchCatInner);
