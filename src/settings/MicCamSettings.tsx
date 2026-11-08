import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Keyboard, Mic } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { ShortcutRecorder } from "./ShortcutRecorder";
import { useT } from "../i18n";

/** Same default as privacy.rs DEFAULT_HOTKEY; an empty value turns meeting mode off. */
export const DEFAULT_MIC_MUTE_HOTKEY = "Win+Ctrl+K";

export function MicCamSettings() {
  const t = useT();
  const [indicator, setIndicator] = useState(() => localStorage.getItem("nectar-mic-cam-indicator") !== "false");
  const [hotkey, setHotkey] = useState(() => localStorage.getItem("nectar-mic-mute-hotkey") ?? DEFAULT_MIC_MUTE_HOTKEY);

  const toggleIndicator = () => {
    const next = !indicator;
    setIndicator(next);
    localStorage.setItem("nectar-mic-cam-indicator", String(next));
    invoke("save_setting", { key: "nectar-mic-cam-indicator", value: String(next) }).catch(() => { });
  };

  const commitHotkey = (next: string) => {
    setHotkey(next);
    localStorage.setItem("nectar-mic-mute-hotkey", next);
    invoke("save_setting", { key: "nectar-mic-mute-hotkey", value: next }).catch(() => { });
  };

  return (
    <>
      <SettingRow icon={Mic} label={t("notch.micCam")} desc={t("notch.micCam.desc")}>
        <label className="toggle-switch">
          <input type="checkbox" checked={indicator} onChange={toggleIndicator} />
          <span className="slider"></span>
        </label>
      </SettingRow>
      <SettingRow icon={Keyboard} label={t("notch.micMute")} desc={t("notch.micMute.desc", { key: DEFAULT_MIC_MUTE_HOTKEY })} divider={false}>
        <ShortcutRecorder value={hotkey} onCommit={commitHotkey} />
      </SettingRow>
    </>
  );
}
