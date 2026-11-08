import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { motion } from "framer-motion";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { dateLocale, useT } from "./i18n";
import { useSettingsSync } from "./hooks/useSettingsSync";
import { setVisibleInterval } from "./hooks/visibleInterval";
import { PlayIcon, PauseIcon } from "./icons";
import { DockFlyout, type DockFlyoutKind, type VolumeState } from "./DockFlyout";
import "./DockSystemArea.css";

/** Settings that gate the system area items. All default to on. */
export const SYSTEM_AREA_KEYS = {
  clock: "nectar-dock-clock-enabled",
  network: "nectar-dock-network-enabled",
  volume: "nectar-dock-volume-enabled",
  notifications: "nectar-dock-notifications-enabled",
  media: "nectar-dock-media-enabled",
} as const;

const NETWORK_POLL_MS = 5000;

interface NetworkStatus {
  kind: "wifi" | "ethernet" | "none";
  signal: number;
}

interface MediaInfo {
  title: string;
  artist: string;
  is_playing: boolean;
  has_media: boolean;
  artwork?: string[];
}

const readFlag = (key: string, fallback: boolean) => {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v !== "false";
  } catch {
    return fallback;
  }
};

/** A boolean setting that follows live changes from Settings and from the settings file. */
function useSystemFlags() {
  const [flags, setFlags] = useState(() => ({
    clock: readFlag(SYSTEM_AREA_KEYS.clock, true),
    network: readFlag(SYSTEM_AREA_KEYS.network, true),
    volume: readFlag(SYSTEM_AREA_KEYS.volume, true),
    notifications: readFlag(SYSTEM_AREA_KEYS.notifications, true),
    media: readFlag(SYSTEM_AREA_KEYS.media, true),
  }));
  // null = the setting was never written, follow the system 12/24-hour and date format.
  const [hour24, setHour24] = useState<boolean | null>(() => { try { const v = localStorage.getItem("nectar-time-format-24h"); return v === null ? null : v === "true"; } catch { return null; } });

  useEffect(() => {
    // Seed from the settings file, the dock webview's localStorage can be older.
    invoke<Record<string, unknown>>("load_settings")
      .then((s) => {
        const pick = (key: string, fallback: boolean) => {
          const v = s?.[key];
          return v === undefined || v === null ? readFlag(key, fallback) : String(v) !== "false";
        };
        setFlags({
          clock: pick(SYSTEM_AREA_KEYS.clock, true),
          network: pick(SYSTEM_AREA_KEYS.network, true),
          volume: pick(SYSTEM_AREA_KEYS.volume, true),
          notifications: pick(SYSTEM_AREA_KEYS.notifications, true),
          media: pick(SYSTEM_AREA_KEYS.media, true),
        });
        const h = s?.["nectar-time-format-24h"];
        if (h !== undefined && h !== null) setHour24(String(h) === "true");
      })
      .catch(() => {});
  }, []);

  const flag = (name: keyof typeof flags) => (v: unknown) => setFlags((f) => ({ ...f, [name]: v !== false && v !== "false" }));
  useSettingsSync({
    [SYSTEM_AREA_KEYS.clock]: flag("clock"),
    [SYSTEM_AREA_KEYS.network]: flag("network"),
    [SYSTEM_AREA_KEYS.volume]: flag("volume"),
    [SYSTEM_AREA_KEYS.notifications]: flag("notifications"),
    [SYSTEM_AREA_KEYS.media]: flag("media"),
    "nectar-time-format-24h": (v) => setHour24(v === true || v === "true"),
  });
  return { flags, hour24 };
}

/** Current time, refreshed on the minute. No timer exists while `active` is false (dock hidden). */
function useMinuteClock(active: boolean) {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    if (!active) return;
    let timer = 0;
    const tick = () => {
      setNow(new Date());
      const d = new Date();
      timer = window.setTimeout(tick, (60 - d.getSeconds()) * 1000 - d.getMilliseconds() + 20);
    };
    const onVisible = () => { if (!document.hidden) setNow(new Date()); };
    tick();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [active]);
  return now;
}

/** Network state, polled every 5 s only while `active` (dock visible), refreshed once on becoming active. */
function useNetwork(active: boolean) {
  const [status, setStatus] = useState<NetworkStatus | null>(null);
  useEffect(() => {
    if (!active) return;
    let alive = true;
    const refresh = () => {
      invoke<NetworkStatus>("get_network_status")
        .then((s) => { if (alive) setStatus((prev) => (prev && prev.kind === s.kind && prev.signal === s.signal ? prev : s)); })
        .catch(() => {});
    };
    refresh();
    const poll = setVisibleInterval(refresh, NETWORK_POLL_MS);
    const onVisible = () => { if (!document.hidden) refresh(); };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive = false;
      clearInterval(poll);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [active]);
  return status;
}

function validLocale(tag: string): string | null {
  try {
    const t = tag.split("_")[0];
    return Intl.DateTimeFormat.supportedLocalesOf(t).length > 0 ? t : null;
  } catch {
    return null;
  }
}

/** Windows regional format locale tag (e.g. "tr-TR"), or null until known / when unavailable. */
function useFormatLocale() {
  const [loc, setLoc] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    invoke<string>("get_system_format_locale").then((v) => { if (alive && v) setLoc(validLocale(v)); }).catch(() => {});
    return () => { alive = false; };
  }, []);
  return loc;
}

function useVolume(enabled: boolean) {
  const [state, setState] = useState<VolumeState | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    invoke<VolumeState>("get_volume_state").then((s) => { if (alive) setState(s); }).catch(() => {});
    const unlisten = listen<VolumeState>("volume-change", (e) => setState(e.payload));
    return () => {
      alive = false;
      unlisten.then((f) => f());
    };
  }, [enabled]);
  return state;
}

function useMedia(enabled: boolean) {
  const [info, setInfo] = useState<MediaInfo | null>(null);
  const artRef = useRef<string | undefined>(undefined);
  const trackRef = useRef("");
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    const apply = (m: MediaInfo | null | undefined) => {
      if (!alive || !m) return;
      const track = `${m.title}|${m.artist}`;
      let needArt = false;
      if (!m.has_media) {
        artRef.current = undefined;
        trackRef.current = "";
      } else {
        // The worker sends the artwork only when it changes, so a new track with byte-identical art
        // arrives without any: drop the cached cover and ask for the full current info instead.
        if (track !== trackRef.current) {
          artRef.current = undefined;
          needArt = true;
        }
        trackRef.current = track;
        if (m.artwork && m.artwork.length > 0) {
          artRef.current = m.artwork[0];
          needArt = false;
        }
      }
      const art = artRef.current;
      // position_ms ticks every ~2 s and is never shown: skip renders when nothing visible changed.
      setInfo((prev) => {
        if (!m.has_media) return prev === null ? prev : null;
        if (prev && prev.title === m.title && prev.artist === m.artist && prev.is_playing === m.is_playing && prev.artwork?.[0] === art) return prev;
        return { ...m, artwork: art ? [art] : undefined };
      });
      if (needArt) {
        invoke<MediaInfo | null>("get_media_info").then((full) => {
          if (!alive || !full || `${full.title}|${full.artist}` !== trackRef.current) return;
          const a = full.artwork?.[0];
          if (a && a !== artRef.current) apply(full);
        }).catch(() => {});
      }
    };
    invoke<MediaInfo | null>("get_media_info").then(apply).catch(() => {});
    const unlisten = listen<MediaInfo>("media-update", (e) => apply(e.payload));
    return () => {
      alive = false;
      artRef.current = undefined;
      trackRef.current = "";
      unlisten.then((f) => f());
    };
  }, [enabled]);
  return info;
}

const iconProps = {
  width: 18,
  height: 18,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2.2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

/** Wi-Fi fan with four arcs, the arcs above the signal level are dimmed. */
function WifiIcon({ signal, off }: { signal: number; off?: boolean }) {
  const level = off ? 0 : signal >= 75 ? 4 : signal >= 50 ? 3 : signal >= 25 ? 2 : 1;
  const arc = (n: number, d: string) => <path key={n} d={d} opacity={level >= n ? 1 : 0.25} />;
  return (
    <svg {...iconProps}>
      {arc(4, "M2 8.8a15 15 0 0 1 20 0")}
      {arc(3, "M5 12.5a10.5 10.5 0 0 1 14 0")}
      {arc(2, "M8.5 16a5.5 5.5 0 0 1 7 0")}
      <circle cx="12" cy="19.5" r="1.1" fill="currentColor" opacity={level >= 1 ? 1 : 0.25} />
      {off && <line x1="3" y1="3" x2="21" y2="21" />}
    </svg>
  );
}

function EthernetIcon() {
  return (
    <svg {...iconProps}>
      <rect x="4" y="3.5" width="16" height="11" rx="2.5" />
      <path d="M8 8v3M12 8v3M16 8v3" />
      <path d="M12 14.5V20M8 20h8" />
    </svg>
  );
}

function VolumeGlyph({ level, muted }: { level: number; muted: boolean }) {
  return (
    <svg {...iconProps}>
      <path d="M4 9.5v5h3.5L12 18.5v-13L7.5 9.5H4z" fill="currentColor" />
      {muted ? (
        <>
          <line x1="16" y1="9.5" x2="21" y2="14.5" />
          <line x1="21" y1="9.5" x2="16" y2="14.5" />
        </>
      ) : (
        <>
          {level > 0 && <path d="M15.5 9.5a3.5 3.5 0 0 1 0 5" />}
          {level > 0.5 && <path d="M18.3 6.8a7.5 7.5 0 0 1 0 10.4" />}
        </>
      )}
    </svg>
  );
}

function BellIcon() {
  return (
    <svg {...iconProps}>
      <path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15L6 16.5z" />
      <path d="M10 21a2.2 2.2 0 0 0 4 0" />
    </svg>
  );
}

function ChevronUpIcon() {
  return (
    <svg {...iconProps} strokeWidth={2.6}>
      <polyline points="6 15 12 9 18 15" />
    </svg>
  );
}

function Slot({ label, onClick, className, btnRef, expanded, children }: {
  label: string;
  onClick: (e: React.MouseEvent<HTMLDivElement>) => void;
  className?: string;
  btnRef?: React.Ref<HTMLDivElement>;
  /** Set for the icons that open a flyout: whether it's open. */
  expanded?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="dock-icon-wrapper sys-slot">
      <div className={`tooltip${expanded ? " tooltip-hidden" : ""}`}>{label}</div>
      <div
        ref={btnRef}
        className={`sys-btn ${className ?? ""}${expanded ? " sys-btn-open" : ""}`}
        role="button"
        tabIndex={-1}
        aria-label={label}
        aria-haspopup={expanded === undefined ? undefined : "dialog"}
        aria-expanded={expanded}
        onClick={(e) => { e.stopPropagation(); onClick(e); }}
        onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
      >
        {children}
      </div>
    </div>
  );
}

interface Props {
  /** The existing `nectar-dock-tray-enabled` setting, owned by the dock. */
  showTray: boolean;
  /** The dock is slid away (smart/peek, fullscreen, shell surface) or collapsed: no timers run. */
  hidden?: boolean;
  /** Open network / sound flyout. The dock owns it so it can stay shown while one is open. */
  flyout: DockFlyoutKind | null;
  onFlyoutChange: (kind: DockFlyoutKind | null) => void;
  /** `nectar-scale`, the flyout is zoomed by it like the dock's context menu. */
  scale: number;
}

/**
 * Right-hand end of the dock, after the app icons: mini player, hidden-icons arrow, network,
 * volume, clock with date and the notification bell. Not rendered in Classic Mode.
 */
export function DockSystemArea({ showTray, hidden = false, flyout, onFlyoutChange, scale }: Props) {
  const t = useT();
  const { flags, hour24 } = useSystemFlags();
  const now = useMinuteClock(flags.clock && !hidden);
  const network = useNetwork(flags.network && !hidden);
  const volume = useVolume(flags.volume);
  const media = useMedia(flags.media);
  const networkRef = useRef<HTMLDivElement>(null);
  const volumeRef = useRef<HTMLDivElement>(null);

  // The icon a flyout belongs to was turned off in Settings.
  useEffect(() => {
    if ((flyout === "network" && !flags.network) || (flyout === "sound" && !flags.volume)) onFlyoutChange(null);
  }, [flyout, flags.network, flags.volume, onFlyoutChange]);

  const toggleFlyout = (kind: DockFlyoutKind) => onFlyoutChange(flyout === kind ? null : kind);
  const flyoutAnchor = flyout === "network" ? networkRef.current : flyout === "sound" ? volumeRef.current : null;

  // Date and (while the 12/24-hour setting is unset) the hour cycle follow the Windows regional
  // format locale (Region > Formats), not the WebView display language.
  const formatLocale = useFormatLocale();
  const hour12 = hour24 === null ? undefined : !hour24;
  const locale = formatLocale ?? dateLocale();
  const timeText = now.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit", hour12 });
  const dateText = now.toLocaleDateString(locale, { day: "numeric", month: "numeric", year: "numeric" });

  const showMedia = flags.media && !!media;
  const showCluster = showTray || flags.network || flags.volume;
  if (!showMedia && !showCluster && !flags.clock && !flags.notifications) return null;

  const networkLabel = !network ? t("dock.sys.network")
    : network.kind === "wifi" ? `${t("dock.sys.network")}, Wi-Fi ${network.signal}%`
    : network.kind === "ethernet" ? `${t("dock.sys.network")}, Ethernet`
    : `${t("dock.sys.network")}, ${t("dock.sys.offline")}`;
  const volPct = volume ? Math.round(volume.volume * 100) : null;
  const volumeLabel = volPct === null ? t("dock.sys.volume") : volume!.is_muted ? `${t("dock.sys.volume")}, ${t("dock.sys.muted")}` : `${t("dock.sys.volume")}, ${volPct}%`;

  return (
    <motion.div
      className="dock-system-area"
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{ opacity: 1, scale: 1 }}
      transition={{ opacity: { duration: 0.15, delay: 0.25 }, scale: { type: "spring", stiffness: 400, damping: 25, delay: 0.25 } }}
    >
      <div className="sys-divider" />

      {showMedia && media && (
        <div className="sys-media">
          <Slot label={media.artist ? `${media.title} · ${media.artist}` : media.title} onClick={() => invoke("open_media_source_app").catch(() => {})} className="sys-art">
            {media.artwork?.[0] ? <img src={media.artwork[0]} alt="" draggable={false} /> : <span className="sys-art-fallback">♪</span>}
          </Slot>
          <Slot label={media.is_playing ? t("dock.sys.pause") : t("dock.sys.play")} onClick={() => invoke("media_play_pause").catch(() => {})}>
            {media.is_playing ? <PauseIcon size={13} /> : <PlayIcon size={13} />}
          </Slot>
        </div>
      )}

      {showTray && (
        <Slot
          label={t("dockmenu.hiddenIcons")}
          onClick={(e) => {
            const rect = e.currentTarget.getBoundingClientRect();
            invoke("open_tray_from_button", {
              dx: (rect.left + rect.width / 2 - e.clientX) * window.devicePixelRatio,
              dy: (rect.top - e.clientY) * window.devicePixelRatio,
            }).catch((err) => console.error("Failed to open tray:", err));
          }}
        >
          <ChevronUpIcon />
        </Slot>
      )}

      {(flags.network || flags.volume) && (
        <div className="sys-cluster">
          {flags.network && (
            <Slot label={networkLabel} btnRef={networkRef} expanded={flyout === "network"} onClick={() => toggleFlyout("network")}>
              {network?.kind === "ethernet" ? <EthernetIcon /> : <WifiIcon signal={network?.signal ?? 100} off={network?.kind === "none"} />}
            </Slot>
          )}
          {flags.volume && (
            <Slot label={volumeLabel} btnRef={volumeRef} expanded={flyout === "sound"} onClick={() => toggleFlyout("sound")}>
              <VolumeGlyph level={volume?.volume ?? 1} muted={!!volume?.is_muted || volume?.volume === 0} />
            </Slot>
          )}
        </div>
      )}

      {flags.clock && (
        <Slot label={t("dockmenu.calendar")} className="sys-clock" onClick={() => invoke("open_calendar_window").catch((err) => console.error("Failed to open calendar:", err))}>
          <span className="sys-time">{timeText}</span>
          <span className="sys-date">{dateText}</span>
        </Slot>
      )}

      {flags.notifications && (
        <Slot label={t("dock.sys.notifications")} onClick={() => invoke("open_notification_panel").catch(() => {})}>
          <BellIcon />
        </Slot>
      )}

      {flyout && flyoutAnchor && (
        <DockFlyout
          kind={flyout}
          anchor={flyoutAnchor}
          scale={scale}
          volume={volume}
          renderVolumeIcon={(level, muted) => <VolumeGlyph level={level} muted={muted || level === 0} />}
          onClose={() => onFlyoutChange(null)}
        />
      )}
    </motion.div>
  );
}
