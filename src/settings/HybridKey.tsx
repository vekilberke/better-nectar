import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Keyboard } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { Dropdown } from "./Dropdown";
import { useT } from "../i18n";

const KEYS = ["altgr", "rctrl", "off"];

export function HybridKey() {
  const t = useT();
  const [value, setValue] = useState(() => {
    const saved = localStorage.getItem("nectar-dock-hybrid-key");
    return saved && KEYS.includes(saved) ? saved : "altgr";
  });

  const change = (next: string) => {
    setValue(next);
    localStorage.setItem("nectar-dock-hybrid-key", next);
    invoke("save_setting", { key: "nectar-dock-hybrid-key", value: next }).catch(() => { });
  };

  return (
    <SettingRow icon={Keyboard} label={t("dock.hybridKey")} desc={t("dock.hybridKey.desc")}>
      <Dropdown value={value} onChange={change} options={KEYS.map((k) => ({ value: k, label: t(`dock.hybridKey.${k}`) }))} />
    </SettingRow>
  );
}
