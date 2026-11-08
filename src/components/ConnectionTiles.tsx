import type { CSSProperties, MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import "./ControlCenter.css";

export interface WifiStatus {
  enabled: boolean;
  connected: boolean;
}

/** Optimistic Wi-Fi radio toggle shared by the notch and the dock flyout; rolls back on failure. */
export async function toggleWifiRadio(previous: WifiStatus | null, setStatus: (s: WifiStatus | null) => void, refresh: () => void) {
  const enabled = !(previous?.enabled ?? false);
  setStatus({ enabled, connected: enabled ? (previous?.connected ?? false) : false });
  try {
    await invoke("set_wifi_state", { enabled });
    // The radio and association take a moment to settle.
    setTimeout(refresh, 800);
  } catch (e) {
    setStatus(previous);
    console.error("Failed to toggle WiFi:", e);
  }
}

/** Optimistic Bluetooth radio toggle shared by the notch and the dock flyout; rolls back on failure. */
export async function toggleBluetoothRadio(current: boolean, setEnabled: (enabled: boolean) => void) {
  const enabled = !current;
  setEnabled(enabled);
  try {
    await invoke("set_bluetooth_state", { enabled });
  } catch (e) {
    setEnabled(!enabled);
    console.error("Failed to toggle Bluetooth:", e);
  }
}

export function WifiIcon({ enabled, connected }: { enabled: boolean; connected: boolean }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" opacity={!enabled ? 0.4 : connected ? 1 : 0.7}>
      <path d="M5 12.55a11 11 0 0 1 14.08 0" />
      <path d="M1.42 9a16 16 0 0 1 21.16 0" />
      <path d="M8.53 16.11a6 6 0 0 1 6.95 0" />
      <line x1="12" y1="20" x2="12.01" y2="20" />
    </svg>
  );
}

export function EthernetIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <rect x="4" y="4" width="16" height="11" rx="2" />
      <path d="M8 15v3h8v-3" />
      <line x1="9" y1="8" x2="9" y2="11" />
      <line x1="12" y1="8" x2="12" y2="11" />
      <line x1="15" y1="8" x2="15" y2="11" />
    </svg>
  );
}

export function BluetoothIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <path d="M6.5 6.5l11 11L12 23V1l5.5 5.5-11 11" />
    </svg>
  );
}

/** The "›" on a tile that opens its list. `tip` spreads the caller's tooltip props. */
export interface TileMore {
  tip?: Record<string, unknown>;
  onClick: () => void;
}

interface NetworkTileProps {
  /** Ethernet is connected and Wi-Fi is not, so the tile shows Ethernet. */
  showLan: boolean;
  /** null until the first status read. */
  status: WifiStatus | null;
  onClick: (e: MouseEvent<HTMLDivElement>) => void;
  onContextMenu?: (e: MouseEvent<HTMLDivElement>) => void;
  more?: TileMore;
  style?: CSSProperties;
  className?: string;
}

/** Control Center Wi-Fi / Ethernet pill, used by the notch and the dock's network flyout. */
export function NetworkTile({ showLan, status, onClick, onContextMenu, more, style, className }: NetworkTileProps) {
  const t = useT();
  const enabled = status?.enabled ?? false;
  const connected = status?.connected ?? false;
  return (
    <div
      className={`cc-pill-tile ${showLan || enabled ? 'active' : ''}${className ? ` ${className}` : ''}`}
      style={style}
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      <div className="cc-pill-icon-wrapper">
        {showLan ? <EthernetIcon /> : <WifiIcon enabled={enabled} connected={connected} />}
      </div>
      <div className="cc-pill-info">
        <span className="cc-pill-title">{showLan ? t('cc.ethernet') : t('cc.wifi')}</span>
        <span className="cc-pill-status">{showLan ? t('connections.connected') : status === null ? '…' : !enabled ? t('common.off') : connected ? t('connections.connected') : t('cc.notConnected')}</span>
      </div>
      {more && (
        <span className="cc-pill-more" {...more.tip} onClick={(e) => { e.stopPropagation(); more.onClick(); }}>›</span>
      )}
    </div>
  );
}

interface BluetoothTileProps {
  enabled: boolean;
  onClick: (e: MouseEvent<HTMLDivElement>) => void;
  onContextMenu?: (e: MouseEvent<HTMLDivElement>) => void;
  more?: TileMore;
  style?: CSSProperties;
  className?: string;
}

/** Control Center Bluetooth pill, used by the notch and the dock's network flyout. */
export function BluetoothTile({ enabled, onClick, onContextMenu, more, style, className }: BluetoothTileProps) {
  const t = useT();
  return (
    <div
      className={`cc-pill-tile ${enabled ? 'active' : ''}${className ? ` ${className}` : ''}`}
      style={style}
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      <div className="cc-pill-icon-wrapper">
        <BluetoothIcon />
      </div>
      <div className="cc-pill-info">
        <span className="cc-pill-title">{t("cc.bluetooth")}</span>
        <span className="cc-pill-status">{enabled ? t('common.on') : t('common.off')}</span>
      </div>
      {more && (
        <span className="cc-pill-more" {...more.tip} onClick={(e) => { e.stopPropagation(); more.onClick(); }}>›</span>
      )}
    </div>
  );
}
