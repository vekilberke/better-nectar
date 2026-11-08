import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Clock3, Wifi, Volume2, Bell, Music } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { useSettingsSync } from "../hooks/useSettingsSync";
import { useT } from "../i18n";

const ITEMS = [
  { key: "nectar-dock-clock-enabled", icon: Clock3, label: "dock.clock", desc: "dock.clock.desc" },
  { key: "nectar-dock-network-enabled", icon: Wifi, label: "dock.network", desc: "dock.network.desc" },
  { key: "nectar-dock-volume-enabled", icon: Volume2, label: "dock.volume", desc: "dock.volume.desc" },
  { key: "nectar-dock-notifications-enabled", icon: Bell, label: "dock.notifications", desc: "dock.notifications.desc" },
  { key: "nectar-dock-media-enabled", icon: Music, label: "dock.media", desc: "dock.media.desc" },
] as const;

const read = (key: string) => {
  try {
    return localStorage.getItem(key) !== "false";
  } catch {
    return true;
  }
};

/** Toggles for the dock's right-hand system area. Every item defaults to on. */
export function DockSystemAreaSettings() {
  const t = useT();
  const [values, setValues] = useState<Record<string, boolean>>(() => Object.fromEntries(ITEMS.map((i) => [i.key, read(i.key)])));

  useEffect(() => {
    invoke<Record<string, unknown>>("load_settings")
      .then((s) => {
        setValues((prev) => {
          const next = { ...prev };
          for (const { key } of ITEMS) {
            const v = s?.[key];
            if (v !== undefined && v !== null) next[key] = String(v) !== "false";
          }
          return next;
        });
      })
      .catch(() => {});
  }, []);

  useSettingsSync(
    Object.fromEntries(ITEMS.map(({ key }) => [key, (v: unknown) => setValues((prev) => ({ ...prev, [key]: v !== false && v !== "false" }))])),
  );

  const toggle = (key: string) => {
    const next = !values[key];
    setValues((prev) => ({ ...prev, [key]: next }));
    try {
      localStorage.setItem(key, String(next));
    } catch { /* storage can be blocked */ }
    invoke("save_setting", { key, value: String(next) }).catch(console.error);
  };

  return (
    <>
      {ITEMS.map(({ key, icon, label, desc }) => (
        <SettingRow key={key} icon={icon} label={t(label)} desc={t(desc)}>
          <label className="toggle-switch">
            <input type="checkbox" checked={values[key]} onChange={() => toggle(key)} />
            <span className="slider"></span>
          </label>
        </SettingRow>
      ))}
    </>
  );
}
