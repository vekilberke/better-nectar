import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { Bot, Clock, CloudSun, Keyboard, Languages, Monitor, Power } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { Dropdown } from "./Dropdown";
import { LANGUAGES, dateLocale, getLanguagePreference, setLanguagePreference, useT } from "../i18n";
import { APP_NAME } from "../buildFlags";
import { WELCOME_KEY, localeUses24h } from "../welcomeGate";
import { DEFAULT_MIC_MUTE_HOTKEY } from "./MicCamSettings";
import type { useSettings } from "./useSettings";

type Weather = "auto" | "city" | "off";

function save(key: string, value: string) {
  localStorage.setItem(key, value);
  return invoke("save_setting", { key, value }).catch(() => { });
}

function Toggle({ checked, onChange }: { checked: boolean; onChange: () => void }) {
  return (
    <label className="toggle-switch">
      <input type="checkbox" checked={checked} onChange={onChange} />
      <span className="slider"></span>
    </label>
  );
}

/** First start of a fresh install: nothing touches the taskbar or the logon task until this is done. */
export function WelcomeView({ settings, onDone }: { settings: ReturnType<typeof useSettings>; onDone: () => void }) {
  const t = useT();
  const [language, setLanguage] = useState(getLanguagePreference);
  const [dock, setDock] = useState(true);
  const [autostart, setAutostart] = useState(true);
  const [weather, setWeather] = useState<Weather>("auto");
  const [clock24, setClock24] = useState(() => localeUses24h(dateLocale()));
  const [meetingKey, setMeetingKey] = useState(false);
  const [claudePresent, setClaudePresent] = useState(false);
  const [claude, setClaude] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    invoke<boolean>("claude_code_present").then(setClaudePresent).catch(() => { });
  }, []);

  const changeLanguage = (value: string) => {
    setLanguagePreference(value);
    setLanguage(value);
    setClock24(localeUses24h(dateLocale()));
  };

  const finish = async () => {
    setBusy(true);
    await save("nectar-dock-enabled", String(dock));
    await save("nectar-notch-mode", "smart");
    await save("nectar-time-format-24h", String(clock24));
    await save("nectar-mic-mute-hotkey", meetingKey ? DEFAULT_MIC_MUTE_HOTKEY : "");
    await save("nectar-weather-enabled", String(weather !== "off"));
    if (claudePresent) await save("nectar-agent-api", String(claude));
    await invoke("set_autostart", { enabled: autostart }).catch(() => { });
    await save(WELCOME_KEY, "true");
    await emit("welcome-done");
    onDone();
  };

  const languageOptions = [
    { value: "system", label: t("general.language.system") },
    ...Object.entries(LANGUAGES).map(([value, { name }]) => ({ value, label: name })),
  ];

  return (
    <div className="welcome-view">
      <h1 className="about-title">{t("welcome.title", { app: APP_NAME })}</h1>
      <p className="welcome-intro">{t("welcome.intro")}</p>

      <div className="setting-group">
        <SettingRow icon={Languages} label={t("general.language")}>
          <Dropdown value={language} options={languageOptions} onChange={changeLanguage} />
        </SettingRow>
        <SettingRow icon={Monitor} label={t("welcome.dock")} desc={t("welcome.dock.desc")}>
          <Toggle checked={dock} onChange={() => setDock(!dock)} />
        </SettingRow>
        <SettingRow icon={Power} label={t("general.launchAtLogin")} desc={t("welcome.autostart.desc")}>
          <Toggle checked={autostart} onChange={() => setAutostart(!autostart)} />
        </SettingRow>
        <SettingRow icon={Clock} label={t("welcome.clock24")}>
          <Toggle checked={clock24} onChange={() => setClock24(!clock24)} />
        </SettingRow>
        <SettingRow icon={Keyboard} label={t("notch.micMute")} desc={t("welcome.meeting.desc", { key: DEFAULT_MIC_MUTE_HOTKEY })}>
          <Toggle checked={meetingKey} onChange={() => setMeetingKey(!meetingKey)} />
        </SettingRow>
        {claudePresent && (
          <SettingRow icon={Bot} label={t("welcome.claude")} desc={t("welcome.claude.desc")}>
            <Toggle checked={claude} onChange={() => setClaude(!claude)} />
          </SettingRow>
        )}
        <SettingRow icon={CloudSun} label={t("welcome.weather")} desc={t("welcome.weather.desc")} divider={weather === "city"}>
          <Dropdown
            value={weather}
            onChange={(v) => setWeather(v as Weather)}
            options={[
              { value: "auto", label: t("welcome.weather.auto") },
              { value: "city", label: t("welcome.weather.city") },
              { value: "off", label: t("welcome.weather.off") },
            ]}
          />
        </SettingRow>
        {weather === "city" && (
          <div className="manual-city-input">
            <div className="city-input-row">
              <input
                type="text"
                placeholder={t("notch.weather.search")}
                value={settings.cityQuery}
                onChange={(e) => settings.setCityQuery(e.target.value)}
                onFocus={() => settings.citySearchResults.length > 0 && settings.setShowCityDropdown(true)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && settings.citySearchResults.length > 0) {
                    e.preventDefault();
                    settings.selectCity(settings.citySearchResults[0]);
                  }
                }}
              />
            </div>
            {settings.showCityDropdown && settings.citySearchResults.length > 0 && (
              <div className="city-dropdown">
                {settings.citySearchResults.map((city) => (
                  <button
                    key={`${city.name}-${city.latitude}`}
                    className="city-dropdown-item"
                    onMouseDown={(e) => {
                      e.preventDefault();
                      settings.selectCity(city);
                    }}
                  >
                    <span className="city-dropdown-name">{city.name}</span>
                    <span className="city-dropdown-country">{city.country}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>

      <p className="welcome-intro">{t("welcome.undo")}</p>
      <button className="update-pill welcome-done" disabled={busy || (weather === "city" && !settings.cityName)} onClick={finish}>
        {t("welcome.done")}
      </button>
    </div>
  );
}
