import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { motion } from "framer-motion";
import { ArrowDown, Check, FolderOpen } from "lucide-react";
import { useT } from "../i18n";
import { downloadsSummary, formatBytes, formatSpeed, visibleDownloads, type Download } from "../downloads";

/** Downloads the Rust watcher follows (`downloads.rs`); empty while off. */
export function useDownloads(enabled: boolean): Download[] {
  const [list, setList] = useState<Download[]>([]);
  useEffect(() => {
    if (!enabled) {
      setList([]);
      return;
    }
    invoke<Download[]>("downloads_list").then(setList).catch(() => { });
    const unlisten = listen<Download[]>("downloads-changed", (e) => setList(e.payload));
    return () => { unlisten.then((fn) => fn()); };
  }, [enabled]);
  return list;
}

const open = (d: Download, reveal: boolean) => { invoke("downloads_open", { id: d.id, reveal }).catch(() => { }); };

/** Collapsed notch: a ring that spins while a download runs, a check for a moment when it is done. */
export function DownloadIndicator({ list }: { list: Download[] }) {
  const t = useT();
  const { active, done, speed } = downloadsSummary(list);
  const title = active ? `${t("downloads.active", { count: active })} · ${formatSpeed(speed)}` : t("downloads.done");
  return (
    <motion.div
      key="download-indicator"
      className={`download-indicator${active ? " running" : " finished"}`}
      title={title}
      aria-label={title}
      initial={{ scale: 0.6, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      exit={{ scale: 0.6, opacity: 0 }}
      transition={{ type: "spring", stiffness: 500, damping: 30 }}
    >
      {active ? (
        <>
          <svg className="download-ring" viewBox="0 0 16 16" aria-hidden>
            <circle cx="8" cy="8" r="6.5" className="download-ring-track" />
            <circle cx="8" cy="8" r="6.5" className="download-ring-arc" />
          </svg>
          <ArrowDown size={8} strokeWidth={3} />
          {active > 1 && <span className="download-count">{active}</span>}
        </>
      ) : (
        done > 0 && <Check size={11} strokeWidth={3} />
      )}
    </motion.div>
  );
}

/** Expanded notch: running and just finished downloads, on top of the Shelf. */
export function DownloadsList({ list }: { list: Download[] }) {
  const t = useT();
  const { rows, more } = visibleDownloads(list);
  if (rows.length === 0) return null;
  return (
    <div className="downloads-list" onClick={(e) => e.stopPropagation()}>
      {rows.map((d) => (
        <div
          key={d.id}
          className={`download-row${d.done ? " done" : ""}`}
          role="button"
          tabIndex={0}
          title={d.done ? t("downloads.open") : t("downloads.reveal")}
          onClick={() => open(d, false)}
          onKeyDown={(e) => { if (e.key === "Enter") open(d, false); }}
        >
          <span className="download-icon">{d.done ? <Check size={12} strokeWidth={2.6} /> : <ArrowDown size={12} strokeWidth={2.6} />}</span>
          <span className="download-text">
            <span className="download-name">{d.name}</span>
            <span className="download-meta">
              {d.done ? `${t("downloads.downloaded")} · ${formatBytes(d.bytes)}` : `${formatBytes(d.bytes)}${d.speed > 0 ? ` · ${formatSpeed(d.speed)}` : ""}`}
            </span>
            {!d.done && <span className="download-bar" aria-hidden />}
          </span>
          <button
            className="download-reveal"
            title={t("downloads.reveal")}
            aria-label={t("downloads.reveal")}
            onClick={(e) => { e.stopPropagation(); open(d, true); }}
          >
            <FolderOpen size={12} strokeWidth={2} />
          </button>
        </div>
      ))}
      {more > 0 && <div className="downloads-more">{t("downloads.more", { count: more })}</div>}
    </div>
  );
}
