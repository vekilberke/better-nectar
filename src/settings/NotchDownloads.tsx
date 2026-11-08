import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Download, FolderOpen } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { useSettingsSync } from "../hooks/useSettingsSync";
import { useT } from "../i18n";

const KEY = "nectar-notch-downloads";
const FOLDER_KEY = "nectar-notch-downloads-folder";

function save(key: string, value: string) {
  localStorage.setItem(key, value);
  invoke("save_setting", { key, value }).catch(() => { });
}

/** Settings › Notch › Controls and shelf: downloads in the notch and the folder they are watched in. */
export function NotchDownloads({ legacyMode }: { legacyMode: boolean }) {
  const t = useT();
  const [enabled, setEnabled] = useState(() => localStorage.getItem(KEY) !== "false");
  const [folder, setFolder] = useState(() => localStorage.getItem(FOLDER_KEY) ?? "");

  useSettingsSync({
    [KEY]: (v) => setEnabled(v !== false),
    [FOLDER_KEY]: (v) => setFolder(v ? String(v) : ""),
  });

  const toggle = () => {
    const next = !enabled;
    setEnabled(next);
    save(KEY, String(next));
  };

  const choose = async () => {
    const picked = await open({ directory: true, multiple: false, defaultPath: folder || undefined }).catch(() => null);
    if (typeof picked !== "string") return;
    setFolder(picked);
    save(FOLDER_KEY, picked);
  };

  const reset = () => {
    setFolder("");
    save(FOLDER_KEY, "");
  };

  if (legacyMode) {
    return <SettingRow icon={Download} label={t("notch.downloads")} desc={t("notch.downloads.legacy")} />;
  }

  return (
    <>
      <SettingRow icon={Download} label={t("notch.downloads")} desc={t("notch.downloads.desc")}>
        <label className="toggle-switch">
          <input type="checkbox" checked={enabled} onChange={toggle} />
          <span className="slider"></span>
        </label>
      </SettingRow>
      {enabled && (
        <SettingRow icon={FolderOpen} label={t("notch.downloads.folder")} desc={folder || t("notch.downloads.folder.default")}>
          <div style={{ display: "flex", gap: 6 }}>
            {folder && (
              <button className="agent-sound-btn" onClick={reset}>
                {t("notch.downloads.folder.reset")}
              </button>
            )}
            <button className="agent-sound-btn" onClick={() => { void choose(); }}>
              {t("notch.downloads.folder.choose")}
            </button>
          </div>
        </SettingRow>
      )}
    </>
  );
}
