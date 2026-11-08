import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AppWindow, Bell, BellOff, ShieldCheck } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { useSettingsSync } from "../hooks/useSettingsSync";
import { useT } from "../i18n";

type Access = "allowed" | "denied" | "unspecified" | "unavailable";
interface NotificationApp { aumid: string; name: string; icon?: string | null }

const KEY = "nectar-notch-notifications";
const HIDE_KEY = "nectar-notch-notifications-hide-toasts";
const EXCLUDE_KEY = "nectar-notch-notifications-exclude";

function save(key: string, value: string) {
  localStorage.setItem(key, value);
  invoke("save_setting", { key, value }).catch(() => { });
}

function parseExclude(raw: unknown): string[] {
  try {
    const list = typeof raw === "string" ? JSON.parse(raw) : raw;
    return Array.isArray(list) ? list.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

/** Settings › Notch › Notifications: the switch, Windows access, banner hiding and the app filter. */
export function NotchNotifications({ legacyMode }: { legacyMode: boolean }) {
  const t = useT();
  const [enabled, setEnabled] = useState(() => localStorage.getItem(KEY) === "true");
  const [hide, setHide] = useState(() => localStorage.getItem(HIDE_KEY) === "true");
  const [exclude, setExclude] = useState<string[]>(() => parseExclude(localStorage.getItem(EXCLUDE_KEY)));
  const [access, setAccess] = useState<Access | null>(null);
  const [apps, setApps] = useState<NotificationApp[] | null>(null);
  const [loadingApps, setLoadingApps] = useState(false);

  useSettingsSync({
    [KEY]: (v) => setEnabled(v === true),
    [HIDE_KEY]: (v) => setHide(v === true),
    [EXCLUDE_KEY]: (v) => setExclude(parseExclude(v)),
  });

  const refreshAccess = () => invoke<Access>("notch_notifications_access").then(setAccess).catch(() => setAccess("unavailable"));

  useEffect(() => {
    if (!enabled) return;
    refreshAccess();
    // The user may change it in Windows Settings and come back.
    window.addEventListener("focus", refreshAccess);
    return () => window.removeEventListener("focus", refreshAccess);
  }, [enabled]);

  const toggle = () => {
    const next = !enabled;
    setEnabled(next);
    save(KEY, String(next));
  };

  const toggleHide = () => {
    const next = !hide;
    setHide(next);
    save(HIDE_KEY, String(next));
  };

  const askAccess = () => {
    if (access === "denied") {
      invoke("notch_notifications_open_privacy").catch(() => { });
      return;
    }
    // No prompt (already decided, or Windows didn't show one): send the user to the Settings page instead.
    invoke<Access>("notch_notifications_request_access")
      .then((status) => {
        setAccess(status);
        if (status !== "allowed") invoke("notch_notifications_open_privacy").catch(() => { });
      })
      .catch(() => { });
  };

  const loadApps = () => {
    setLoadingApps(true);
    invoke<NotificationApp[]>("notch_notification_apps")
      .then(setApps)
      .catch(() => setApps([]))
      .finally(() => setLoadingApps(false));
  };

  const toggleApp = (aumid: string) => {
    const lower = aumid.toLowerCase();
    const next = exclude.some((e) => e.toLowerCase() === lower) ? exclude.filter((e) => e.toLowerCase() !== lower) : [...exclude, aumid];
    setExclude(next);
    save(EXCLUDE_KEY, JSON.stringify(next));
  };

  if (legacyMode) {
    return <SettingRow icon={Bell} label={t("notch.notifications")} desc={t("notch.notifications.legacy")} divider={false} />;
  }

  return (
    <>
      <SettingRow icon={Bell} label={t("notch.notifications")} desc={t("notch.notifications.desc")} divider={enabled}>
        <label className="toggle-switch">
          <input type="checkbox" checked={enabled} onChange={toggle} />
          <span className="slider"></span>
        </label>
      </SettingRow>
      {enabled && (
        <>
          <SettingRow icon={ShieldCheck} label={t("notch.notifications.access")} desc={access ? t(`notch.notifications.access.${access}`) : "…"}>
            {(access === "denied" || access === "unspecified") && (
              <button className="agent-sound-btn" onClick={askAccess}>
                {t(access === "denied" ? "notch.notifications.access.open" : "notch.notifications.access.ask")}
              </button>
            )}
          </SettingRow>
          <SettingRow icon={BellOff} label={t("notch.notifications.hideToasts")} desc={t("notch.notifications.hideToasts.desc")}>
            <label className="toggle-switch">
              <input type="checkbox" checked={hide} onChange={toggleHide} />
              <span className="slider"></span>
            </label>
          </SettingRow>
          <SettingRow icon={AppWindow} label={t("notch.notifications.apps")} desc={t("notch.notifications.apps.desc")} divider={false} stacked={apps !== null}>
            {apps === null ? (
              <button className="agent-sound-btn" onClick={loadApps} disabled={loadingApps}>
                {t(loadingApps ? "notch.notifications.apps.loading" : "notch.notifications.apps.load")}
              </button>
            ) : apps.length === 0 ? (
              <span className="setting-desc">{t("notch.notifications.apps.empty")}</span>
            ) : (
              <div className="notice-apps">
                {apps.map((app) => {
                  const shown = !exclude.some((e) => e.toLowerCase() === app.aumid.toLowerCase());
                  return (
                    <label key={app.aumid} className="notice-app" title={app.aumid}>
                      <span className="notice-app-icon">{app.icon ? <img src={app.icon} alt="" /> : <Bell size={12} />}</span>
                      <span className="notice-app-name">{app.name}</span>
                      <span className="toggle-switch">
                        <input type="checkbox" checked={shown} onChange={() => toggleApp(app.aumid)} />
                        <span className="slider"></span>
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
          </SettingRow>
        </>
      )}
    </>
  );
}
