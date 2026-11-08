import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, ArrowDownToLine, ArrowUpFromLine, ClipboardCopy, FileText, FileUp, FolderOpen, HardDrive, Link2, QrCode, ShieldCheck, Smartphone, Trash2, Wifi } from "lucide-react";
import { SettingRow } from "./SettingRow";
import { SettingSection } from "./SettingSection";
import { Dropdown } from "./Dropdown";
import { timeAgo, useT, type TFunction } from "../i18n";

export type OpenUrlMode = "off" | "ask" | "on";

export interface PhonePerms {
  media: boolean;
  volume: boolean;
  lock: boolean;
  status: boolean;
  mediaInfo: boolean;
  agentStatus: boolean;
  agentAsk: boolean;
  clipboard: boolean;
  filesIn: boolean;
  textIn: boolean;
  filesOut: boolean;
  textOut: boolean;
  clipboardOut: boolean;
  openUrl: OpenUrlMode;
}

interface DeviceInfo {
  id: string;
  name: string;
  ua: string;
  pairedAt: number;
  lastSeen: number | null;
  perms: PhonePerms;
  suspended: boolean;
  epoch: number;
}

interface SessionInfo {
  deviceId: string;
  startedAt: number;
}

interface Status {
  enabled: boolean;
  state: string;
  detail: string | null;
  devices: DeviceInfo[];
  sessions: SessionInfo[];
  pairing: boolean;
  pairingView: { qrSvg: string; expiresAt: number; request: { name: string; ua: string } | null } | null;
  pendingCleanup: number;
  downloadDir: string;
  maxInMb: number;
  maxOutMb: number;
  defaultPerms: PhonePerms;
  clipSync: boolean;
}

export interface Transfer {
  fid: string;
  deviceId: string;
  dir: "p2c" | "c2p";
  name: string;
  size: number;
  done: number;
  state: string;
  e?: string | null;
}

const FINISHED = new Set(["saved", "queued", "failed"]);
const mb = (n: number) => (n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1);

function transferLine(t: TFunction, x: Transfer, device: string): string {
  const errKey = `phone.xfer.err.${x.e}`;
  const err = x.state === "failed" && x.e ? t(errKey) : "";
  const state = err && err !== errKey ? err : t(`phone.xfer.state.${x.state}`);
  const moving = x.state === "receiving" || x.state === "sending";
  const size = moving && x.size > 0 ? `${mb(x.done)} / ${mb(x.size)} MB` : `${mb(x.size)} MB`;
  return [t(x.dir === "p2c" ? "phone.xfer.from" : "phone.xfer.to", { name: device }), state, size].join(" · ");
}

interface UrlRequest {
  id: string;
  name: string;
  url: string;
  host: string;
}

type BoolPerm = Exclude<keyof PhonePerms, "openUrl">;

const PERM_KEYS: BoolPerm[] = [
  "media", "volume", "lock", "status", "mediaInfo", "agentStatus", "agentAsk",
  "clipboard", "clipboardOut", "textIn", "textOut", "filesIn", "filesOut",
];

const CLIP_SYNC = "nectar-phone-clipboard-sync";

const IN_LIMITS = ["25", "50", "100", "200"];
const OUT_LIMITS = ["10", "25", "50", "100"];

function stateLabel(t: TFunction, state: string, detail: string | null): string {
  if (state === "error" && detail === "anonymous_disabled") return t("phone.state.anonymousDisabled");
  const key = `phone.state.${state}`;
  const label = t(key);
  return label === key ? state : label;
}

function PermEditor({ perms, onChange, t }: { perms: PhonePerms; onChange: (p: PhonePerms) => void; t: TFunction }) {
  return (
    <div className="phone-perms">
      {PERM_KEYS.map((key) => (
        <label key={key} className="phone-perm">
          <input type="checkbox" checked={perms[key]} onChange={(e) => onChange({ ...perms, [key]: e.target.checked })} />
          <span>{t(`phone.perm.${key}`)}</span>
        </label>
      ))}
      <div className="phone-perm phone-perm-url">
        <span>{t("phone.perm.openUrl")}</span>
        <Dropdown
          value={perms.openUrl}
          options={(["on", "ask", "off"] as OpenUrlMode[]).map((v) => ({ value: v, label: t(`phone.openUrl.${v}`) }))}
          onChange={(v) => onChange({ ...perms, openUrl: v as OpenUrlMode })}
        />
      </div>
    </div>
  );
}

export function PhoneLinkTab() {
  const t = useT();
  const [status, setStatus] = useState<Status | null>(null);
  const [qr, setQr] = useState<{ svg: string; expiresAt: number } | null>(null);
  const [request, setRequest] = useState<{ name: string; ua: string } | null>(null);
  const [code, setCode] = useState("");
  const [pairPerms, setPairPerms] = useState<PhonePerms | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [openDevice, setOpenDevice] = useState<string | null>(null);
  const [urlRequests, setUrlRequests] = useState<UrlRequest[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const pairingRef = useRef(false);
  const restoredRef = useRef(false);

  const refresh = useCallback(() => {
    invoke<Status>("phone_link_status")
      .then((s) => {
        setStatus(s);
        // Pairing that started before this tab was mounted (e.g. after switching tabs).
        if (!restoredRef.current) {
          restoredRef.current = true;
          const view = s.pairingView;
          if (view && !pairingRef.current) {
            pairingRef.current = true;
            setQr({ svg: view.qrSvg, expiresAt: view.expiresAt });
            setRequest(view.request);
            setPairPerms(s.defaultPerms);
            setNow(Date.now());
          }
        }
      })
      .catch(() => {});
  }, []);

  const resetPairing = useCallback(() => {
    pairingRef.current = false;
    setQr(null);
    setRequest(null);
    setCode("");
    setPairPerms(null);
  }, []);

  useEffect(() => {
    refresh();
    const subs = [
      listen("phone-link-state", refresh),
      listen("phone-link-devices", refresh),
      listen("phone-link-session", refresh),
      listen("phone-link-cleanup", refresh),
      listen<{ deviceId: string; name: string }>("phone-link-device-suspended", refresh),
      listen<{ name: string; ua: string }>("phone-link-pair-request", (e) => setRequest(e.payload)),
      listen<{ reason: string }>("phone-link-pair-aborted", (e) => {
        if (!pairingRef.current) return;
        resetPairing();
        const key = `phone.aborted.${e.payload.reason}`;
        const text = t(key);
        setMessage(text === key ? t("phone.aborted.relay") : text);
      }),
      listen<UrlRequest>("phone-link-url-request", (e) => setUrlRequests((list) => [...list.slice(-4), e.payload])),
      listen<Transfer>("phone-link-transfer", (e) =>
        setTransfers((list) => {
          const prev = list.find((x) => x.fid === e.payload.fid);
          const next = { ...prev, ...e.payload, done: e.payload.done || prev?.done || 0, name: e.payload.name || prev?.name || "" };
          return [next, ...list.filter((x) => x.fid !== e.payload.fid)].slice(0, 8);
        }),
      ),
      listen<{ key: string }>("settings-changed", (e) => {
        if (e.payload?.key === "nectar-phone-link" || e.payload?.key === CLIP_SYNC) refresh();
      }),
    ];
    return () => subs.forEach((p) => p.then((fn) => fn()));
  }, [refresh, resetPairing, t]);

  useEffect(() => {
    invoke<Transfer[]>("phone_link_transfers")
      .then((list) => setTransfers((cur) => (cur.length ? cur : list.map((x) => ({ ...x, done: 0 })))))
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!qr) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [qr]);

  useEffect(() => {
    if (qr && now / 1000 >= qr.expiresAt) {
      resetPairing();
      setMessage(t("phone.aborted.expired"));
    }
  }, [now, qr, resetPairing, t]);

  const toggle = async () => {
    const next = !(status?.enabled ?? false);
    if (!next) resetPairing();
    await invoke("save_setting", { key: "nectar-phone-link", value: next ? "true" : "false" }).catch(() => {});
    setTimeout(refresh, 150);
  };

  const toggleClipSync = async () => {
    const next = !(status?.clipSync ?? false);
    setStatus((s) => (s ? { ...s, clipSync: next } : s));
    await invoke("save_setting", { key: CLIP_SYNC, value: next ? "true" : "false" }).catch(() => {});
    setTimeout(refresh, 150);
  };

  const startPairing = async () => {
    setMessage(null);
    setBusy(true);
    try {
      const view = await invoke<{ qrSvg: string; expiresAt: number }>("phone_link_start_pairing");
      pairingRef.current = true;
      setQr({ svg: view.qrSvg, expiresAt: view.expiresAt });
      setRequest(null);
      setPairPerms(status?.defaultPerms ?? null);
      setNow(Date.now());
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  const cancelPairing = () => {
    invoke("phone_link_cancel_pairing").catch(() => {});
    resetPairing();
  };

  const submitCode = async () => {
    if (!pairPerms) return;
    setBusy(true);
    setMessage(null);
    try {
      const r = await invoke<{ ok: boolean; left?: number; aborted?: string }>("phone_link_submit_code", { code, perms: pairPerms });
      if (r.ok) {
        resetPairing();
        setMessage(t("phone.paired"));
        refresh();
      } else if (r.left !== undefined) {
        setCode("");
        setMessage(t("phone.wrongCode", { count: r.left }));
      }
    } catch (e) {
      resetPairing();
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  const setPerms = (id: string, perms: PhonePerms) => {
    setStatus((s) => (s ? { ...s, devices: s.devices.map((d) => (d.id === id ? { ...d, perms } : d)) } : s));
    invoke("phone_link_set_permissions", { id, perms }).catch((e) => {
      setMessage(String(e));
      refresh();
    });
  };

  const revoke = (id: string) => {
    if (!window.confirm(t("phone.removeConfirm"))) return;
    invoke("phone_link_revoke", { id }).finally(refresh);
  };

  const revokeAll = () => {
    if (!window.confirm(t("phone.removeAllConfirm"))) return;
    invoke("phone_link_revoke_all").finally(refresh);
  };

  const answerUrl = (req: UrlRequest, open: boolean) => {
    invoke("phone_link_url_answer", { id: req.id, open }).catch(() => {});
    setUrlRequests((list) => list.filter((r) => r.id !== req.id));
  };

  const sendFile = (id: string) => {
    setMessage(null);
    invoke<{ queued?: boolean } | null>("phone_link_send_pick", { deviceId: id })
      .then((r) => r?.queued && setMessage(t("phone.sendQueued")))
      .catch((e) => {
        const key = `phone.xfer.err.${String(e)}`;
        const text = t(key);
        setMessage(text === key ? String(e) : text);
      });
  };

  const changeLimits = (maxInMb: number, maxOutMb: number) => {
    invoke("phone_link_set_limits", { maxInMb, maxOutMb }).then(refresh).catch(() => {});
  };

  const changeDir = () => {
    invoke<string | null>("phone_link_set_download_dir")
      .then(refresh)
      .catch((e) => setMessage(t("phone.dirRejected", { reason: String(e) })));
  };

  const enabled = status?.enabled ?? false;
  const online = status?.state === "online";
  const sessions = new Set((status?.sessions ?? []).map((s) => s.deviceId));
  const secondsLeft = qr ? Math.max(0, Math.round(qr.expiresAt - now / 1000)) : 0;
  const clipNoDevice = !!status?.clipSync && status.devices.length > 0 && !status.devices.some((d) => d.perms.clipboardOut && !d.suspended);
  const clipAllowable = clipNoDevice && status ? status.devices.filter((d) => !d.suspended) : [];

  return (
    <>
      <SettingSection id="phone-main" title={t("phone.group")} defaultOpen>
        <SettingRow icon={Smartphone} label={t("phone.enable")} desc={t("phone.enable.desc")}>
          <label className="toggle-switch">
            <input type="checkbox" checked={enabled} onChange={toggle} />
            <span className="slider"></span>
          </label>
        </SettingRow>
        {enabled && status && (
          <SettingRow icon={Wifi} label={t("phone.connection")} desc={stateLabel(t, status.state, status.detail)}>
            {!online && (
              <button className="caldav-btn" onClick={() => invoke("phone_link_reconnect").catch(() => {})}>
                {t("phone.retry")}
              </button>
            )}
          </SettingRow>
        )}
        {enabled && status && (
          <SettingRow
            icon={ClipboardCopy}
            label={t("phone.clipSync")}
            desc={clipNoDevice ? t(clipAllowable.length ? "phone.clipSync.noDevice" : "phone.clipSync.allSuspended") : t("phone.clipSync.desc")}
            divider={false}
          >
            <label className="toggle-switch">
              <input type="checkbox" checked={status.clipSync} onChange={toggleClipSync} />
              <span className="slider"></span>
            </label>
          </SettingRow>
        )}
        {enabled && clipAllowable.length > 0 && (
          <div className="phone-allow-row">
            {clipAllowable.map((d) => (
              <button key={d.id} className="caldav-btn" onClick={() => setPerms(d.id, { ...d.perms, clipboardOut: true })}>
                {t("phone.clipSync.allow", { name: d.name })}
              </button>
            ))}
          </div>
        )}
      </SettingSection>

      {message && <div className="caldav-error caldav-error--page">{message}</div>}

      {urlRequests.map((req) => (
        <div key={req.id} className="phone-url-request">
          <Link2 size={14} strokeWidth={1.5} />
          <span>{t("phone.urlRequest", { name: req.name, host: req.host })}</span>
          <button className="caldav-btn primary" onClick={() => answerUrl(req, true)}>{t("phone.urlOpen")}</button>
          <button className="caldav-btn" onClick={() => answerUrl(req, false)}>{t("phone.urlIgnore")}</button>
        </div>
      ))}

      {enabled && (
        <SettingSection id="phone-pair" title={t("phone.pairGroup")} defaultOpen>
          {!qr && (
            <SettingRow icon={QrCode} label={t("phone.pair")} desc={t("phone.pair.desc")} divider={false}>
              <button className="caldav-btn primary" disabled={!online || busy} onClick={startPairing}>
                {t("phone.pairButton")}
              </button>
            </SettingRow>
          )}
          {qr && !request && (
            <div className="phone-qr-box">
              <img className="phone-qr" src={qr.svg} alt={t("phone.qrAlt")} />
              <div className="phone-qr-side">
                <span className="caldav-hint">{t("phone.qrHelp")}</span>
                <span className="caldav-hint">{t("phone.expiresIn", { seconds: secondsLeft })}</span>
                <button className="caldav-btn" onClick={cancelPairing}>{t("phone.cancel")}</button>
              </div>
            </div>
          )}
          {qr && request && pairPerms && (
            <div className="caldav-form">
              <span className="phone-request">{t("phone.request", { name: request.name || "?" })}</span>
              {request.ua && <span className="caldav-hint">{request.ua}</span>}
              <input
                className="phone-code"
                inputMode="numeric"
                autoFocus
                maxLength={6}
                placeholder="000000"
                value={code}
                onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
                onKeyDown={(e) => e.key === "Enter" && code.length === 6 && submitCode()}
              />
              <span className="caldav-hint">{t("phone.permsAtPairing")}</span>
              <PermEditor perms={pairPerms} onChange={setPairPerms} t={t} />
              <div className="caldav-form-footer">
                <span className="caldav-hint">{t("phone.expiresIn", { seconds: secondsLeft })}</span>
                <div className="caldav-actions">
                  <button className="caldav-btn" onClick={cancelPairing}>{t("phone.cancel")}</button>
                  <button className="caldav-btn primary" disabled={code.length !== 6 || busy} onClick={submitCode}>
                    {t("phone.pairConfirm")}
                  </button>
                </div>
              </div>
            </div>
          )}
        </SettingSection>
      )}

      {(status?.devices.length ?? 0) > 0 && (
        <SettingSection id="phone-devices" title={t("phone.devicesGroup")} defaultOpen>
          {status!.devices.map((d) => (
            <div key={d.id}>
              <SettingRow
                icon={d.suspended ? AlertTriangle : Smartphone}
                label={d.name}
                desc={
                  d.suspended
                    ? t("phone.suspended")
                    : sessions.has(d.id)
                      ? t("phone.connectedNow")
                      : t("phone.lastSeen", { when: timeAgo(d.lastSeen ? d.lastSeen * 1000 : null, t) })
                }
                divider={false}
              >
                <div className="caldav-actions">
                  {enabled && d.perms.filesOut && !d.suspended && (
                    <button className="caldav-btn" title={t("phone.sendFile.desc")} onClick={() => sendFile(d.id)}>
                      <FileUp size={13} strokeWidth={1.75} />
                      {t("phone.sendFile")}
                    </button>
                  )}
                  {sessions.has(d.id) && (
                    <button className="caldav-btn" onClick={() => invoke("phone_link_end_session", { id: d.id }).finally(refresh)}>
                      {t("phone.endSession")}
                    </button>
                  )}
                  <button className="caldav-btn" onClick={() => setOpenDevice(openDevice === d.id ? null : d.id)}>
                    {t("phone.permissions")}
                  </button>
                  <button className="caldav-btn danger" onClick={() => revoke(d.id)}>
                    {t("phone.remove")}
                  </button>
                </div>
              </SettingRow>
              {openDevice === d.id && (
                <div className="caldav-form">
                  <PermEditor perms={d.perms} onChange={(p) => setPerms(d.id, p)} t={t} />
                </div>
              )}
              <div className="setting-divider" />
            </div>
          ))}
          {status!.pendingCleanup > 0 && <div className="caldav-hint phone-note">{t("phone.cleanupPending")}</div>}
          <SettingRow icon={Trash2} label={t("phone.removeAll")} danger action onClick={revokeAll} divider={false} />
        </SettingSection>
      )}

      {transfers.length > 0 && (
        <SettingSection id="phone-transfers" title={t("phone.xfer.group")} defaultOpen>
          {transfers.map((x, i) => {
            const device = status?.devices.find((d) => d.id === x.deviceId)?.name ?? "";
            const pct = x.size > 0 ? Math.min(100, Math.round((x.done / x.size) * 100)) : 0;
            return (
              <SettingRow
                key={x.fid}
                icon={x.dir === "p2c" ? ArrowDownToLine : ArrowUpFromLine}
                label={x.name || t("phone.xfer.text")}
                desc={transferLine(t, x, device)}
                danger={x.state === "failed"}
                divider={i < transfers.length - 1}
              >
                {!FINISHED.has(x.state) && (
                  <div className="phone-xfer-bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
                    <span style={{ width: `${x.state === "receiving" || x.state === "sending" ? pct : 100}%` }} />
                  </div>
                )}
                {FINISHED.has(x.state) && (
                  <button className="caldav-btn" onClick={() => setTransfers((list) => list.filter((y) => y.fid !== x.fid))}>
                    {t("phone.xfer.clear")}
                  </button>
                )}
              </SettingRow>
            );
          })}
        </SettingSection>
      )}

      {status && (
        <SettingSection id="phone-files" title={t("phone.filesGroup")}>
          <SettingRow icon={FolderOpen} label={t("phone.downloadDir")} desc={status.downloadDir}>
            <button className="caldav-btn" onClick={changeDir}>{t("phone.change")}</button>
          </SettingRow>
          <SettingRow icon={HardDrive} label={t("phone.maxIn")} desc={t("phone.maxIn.desc")}>
            <Dropdown
              value={String(status.maxInMb)}
              options={IN_LIMITS.map((v) => ({ value: v, label: `${v} MB` }))}
              onChange={(v) => changeLimits(Number(v), status.maxOutMb)}
            />
          </SettingRow>
          <SettingRow icon={HardDrive} label={t("phone.maxOut")} desc={t("phone.maxOut.desc")}>
            <Dropdown
              value={String(status.maxOutMb)}
              options={OUT_LIMITS.map((v) => ({ value: v, label: `${v} MB` }))}
              onChange={(v) => changeLimits(status.maxInMb, Number(v))}
            />
          </SettingRow>
          <SettingRow icon={FileText} label={t("phone.audit")} desc={t("phone.audit.desc")} action onClick={() => invoke("phone_link_open_audit_log").catch(() => {})} />
          <SettingRow icon={ShieldCheck} label={t("phone.privacy")} desc={t("phone.privacy.desc")} divider={false} />
        </SettingSection>
      )}
    </>
  );
}
