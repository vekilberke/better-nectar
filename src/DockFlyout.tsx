import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";
import { motion } from "framer-motion";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useT } from "./i18n";
import { setVisibleInterval } from "./hooks/visibleInterval";
import { useTrailingThrottle } from "./hooks/useTrailingThrottle";
import { ConnectionsView } from "./components/ConnectionsView";
import { BluetoothTile, NetworkTile, toggleBluetoothRadio, toggleWifiRadio, type WifiStatus } from "./components/ConnectionTiles";
import "./components/ControlCenter.css";
import "./DockFlyout.css";

export type DockFlyoutKind = "network" | "sound";

export interface VolumeState {
  volume: number;
  is_muted: boolean;
}

/** Card width in CSS px before the Nectar scale (the card is zoomed like the dock's context menu). */
const FLYOUT_WIDTH = 330;
/** Gap between the dock's top edge and the card, and the minimum distance to the window edges. */
const GAP = 10;
const EDGE = 8;

const tip = (text: string | undefined) => (text ? { title: text } : {});

interface Placement {
  left: number;
  bottom: number;
  maxHeight: number;
}

/**
 * Network or sound menu above the dock, built from the notch's Control Center pieces. It lives in the
 * dock window (portaled to <body>, the dock pill is transformed), so it follows that dock's monitor and
 * the adaptive width. Nothing here runs while it's closed: the parent unmounts it.
 */
export function DockFlyout({ kind, anchor, scale, volume, renderVolumeIcon, onClose }: {
  kind: DockFlyoutKind;
  /** The dock icon that opened it, the card is centered on it. */
  anchor: HTMLElement;
  scale: number;
  volume: VolumeState | null;
  renderVolumeIcon: (level: number, muted: boolean) => ReactNode;
  onClose: () => void;
}) {
  const t = useT();
  const wrapRef = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState<Placement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  // Anchor to the icon, above the dock's top edge, kept inside the window (= the monitor's width).
  useLayoutEffect(() => {
    const dock = anchor.closest(".dock") as HTMLElement | null;
    const update = () => {
      const a = anchor.getBoundingClientRect();
      const top = (dock ?? anchor).getBoundingClientRect().top;
      const width = wrapRef.current?.getBoundingClientRect().width || FLYOUT_WIDTH * scale;
      const left = Math.min(Math.max(EDGE, a.left + a.width / 2 - width / 2), Math.max(EDGE, window.innerWidth - width - EDGE));
      const next = {
        left: Math.round(left),
        bottom: Math.round(window.innerHeight - top + GAP),
        maxHeight: Math.max(160, Math.floor((top - GAP - EDGE) / (scale || 1))),
      };
      setPlace((prev) => (prev && prev.left === next.left && prev.bottom === next.bottom && prev.maxHeight === next.maxHeight ? prev : next));
    };
    update();
    const observer = new ResizeObserver(update);
    if (dock) observer.observe(dock);
    if (wrapRef.current) observer.observe(wrapRef.current);
    window.addEventListener("resize", update);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [anchor, scale]);

  // Tell the backend where the card is: it stays clickable there, and a press elsewhere or Esc closes it.
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || !place) return;
    let last = "";
    const report = () => {
      const r = el.getBoundingClientRect();
      const rect = { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) };
      const key = `${rect.x},${rect.y},${rect.width},${rect.height}`;
      if (key === last) return;
      last = key;
      invoke("set_dock_flyout", { rect }).catch(() => { last = ""; });
    };
    report();
    const observer = new ResizeObserver(report);
    observer.observe(el);
    return () => observer.disconnect();
  }, [place]);

  useEffect(() => () => { invoke("set_dock_flyout", { rect: null }).catch(() => {}); }, []);

  useEffect(() => {
    const close = () => onCloseRef.current();
    const unlisten = getCurrentWebviewWindow().listen("dock-flyout-dismiss", close);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") close(); };
    const onVisibility = () => { if (document.hidden) close(); };
    window.addEventListener("keydown", onKey);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      unlisten.then((f) => f());
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  return createPortal(
    <div
      ref={wrapRef}
      className="dock-flyout-wrap"
      style={place ? { left: place.left, bottom: place.bottom } : { left: -10000, bottom: GAP }}
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
      onWheel={(e) => e.stopPropagation()}
    >
      <motion.div
        className="dock-flyout"
        role="dialog"
        aria-label={t(kind === "network" ? "dock.flyout.network" : "dock.flyout.sound")}
        style={{ zoom: scale, width: FLYOUT_WIDTH, maxHeight: place?.maxHeight }}
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 500, damping: 34 }}
      >
        {kind === "network" ? <NetworkPanel onClose={onClose} /> : <SoundPanel volume={volume} renderVolumeIcon={renderVolumeIcon} onClose={onClose} />}
      </motion.div>
    </div>,
    document.body,
  );
}

/** The notch's Wi-Fi and Bluetooth tiles on top, the chosen one's list below. */
function NetworkPanel({ onClose }: { onClose: () => void }) {
  const t = useT();
  const [wifi, setWifi] = useState<WifiStatus | null>(null);
  const [lanConnected, setLanConnected] = useState(false);
  const [bluetooth, setBluetooth] = useState<boolean | null>(null);
  const [list, setList] = useState<"wifi" | "bluetooth">("wifi");

  const refreshWifi = useCallback(() => {
    invoke<WifiStatus>("get_wifi_status").then(setWifi).catch(() => {});
    invoke<{ connected: boolean }>("get_lan_status").then((s) => setLanConnected(!!s?.connected)).catch(() => {});
  }, []);

  useEffect(() => {
    refreshWifi();
    invoke<boolean>("get_bluetooth_state").then(setBluetooth).catch(() => {});
    // Same cadence as the notch's Control Center: the radios can change from the keyboard or Windows.
    const timer = setVisibleInterval(refreshWifi, 5000);
    return () => clearInterval(timer);
  }, [refreshWifi]);

  const showLan = lanConnected && !(wifi?.enabled && wifi.connected);

  const toggleWifi = () => toggleWifiRadio(wifi, setWifi, refreshWifi);
  const toggleBluetooth = () => toggleBluetoothRadio(!!bluetooth, setBluetooth);

  return (
    <>
      <div className="cc-pills-grid">
        <NetworkTile
          showLan={showLan}
          status={wifi}
          className={list === "wifi" ? "dfly-selected" : undefined}
          onClick={() => { if (showLan) { invoke("open_ethernet_settings").catch(() => {}); onClose(); } else void toggleWifi(); }}
          more={{ tip: tip(t("cc.networks")), onClick: () => setList("wifi") }}
        />
        <BluetoothTile
          enabled={!!bluetooth}
          className={list === "bluetooth" ? "dfly-selected" : undefined}
          onClick={() => void toggleBluetooth()}
          more={{ tip: tip(t("cc.devices")), onClick: () => setList("bluetooth") }}
        />
      </div>
      <div className="dfly-list">
        <ConnectionsView
          key={list}
          kind={list}
          onOpenWindowsSettings={() => { invoke(list === "wifi" ? "open_wifi_settings" : "open_bluetooth_settings").catch(() => {}); onClose(); }}
          tip={tip}
        />
      </div>
    </>
  );
}

/** Master volume with mute, then the notch's output device list and per-app mixer. */
function SoundPanel({ volume, renderVolumeIcon, onClose }: { volume: VolumeState | null; renderVolumeIcon: (level: number, muted: boolean) => ReactNode; onClose: () => void }) {
  const t = useT();
  const [level, setLevel] = useState(volume?.volume ?? 0);
  const [muted, setMuted] = useState(volume?.is_muted ?? false);
  const dragging = useRef(false);

  useEffect(() => {
    if (!volume || dragging.current) return;
    setLevel(volume.volume);
    setMuted(volume.is_muted);
  }, [volume]);

  const sendVolume = useTrailingThrottle((v: number) => {
    invoke("set_volume", { volume: v }).catch(() => {});
  }, 50);

  const change = (v: number) => {
    setLevel(v);
    // set_volume unmutes for any non-zero level.
    if (v > 0) setMuted(false);
    sendVolume(v);
  };

  const toggleMute = () => {
    const next = !muted;
    setMuted(next);
    invoke("set_volume_muted", { muted: next }).catch(() => setMuted(!next));
  };

  // Sliders show 0 while muted, like the notch.
  const shown = muted ? 0 : level;
  const pct = Math.round(shown * 100);

  return (
    <>
      <div
        className="cc-classic-slider-row dfly-volume"
        onWheel={(e) => { e.stopPropagation(); change(Math.min(1, Math.max(0, Math.round((shown + (e.deltaY < 0 ? 0.02 : -0.02)) * 100) / 100))); }}
      >
        <button
          className={`cc-circular-btn dfly-mute${muted ? " active" : ""}`}
          title={muted ? t("overlay.unmute") : t("overlay.mute")}
          aria-label={muted ? t("overlay.unmute") : t("overlay.mute")}
          aria-pressed={muted}
          onClick={toggleMute}
        >
          {renderVolumeIcon(level, muted)}
        </button>
        <div className="cc-classic-slider-track">
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            value={shown}
            aria-label={t("cc.volume")}
            className="cc-classic-input"
            onChange={(e) => change(parseFloat(e.target.value))}
            onPointerDown={() => { dragging.current = true; }}
            onPointerUp={() => { dragging.current = false; }}
            onPointerCancel={() => { dragging.current = false; }}
          />
          <div className="cc-classic-fill" style={{ width: `${shown * 100}%` }} />
        </div>
        <span className="cc-classic-percentage">{pct}%</span>
      </div>
      <div className="dfly-list dfly-list-sound">
        <ConnectionsView
          kind="audio"
          onOpenWindowsSettings={() => { invoke("open_sound_settings").catch(() => {}); onClose(); }}
          tip={tip}
        />
      </div>
    </>
  );
}
