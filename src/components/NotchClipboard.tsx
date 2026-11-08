import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, MotionConfig, motion } from "framer-motion";
import { Check, ClipboardList, File, Files, Image, Link2, Type, X } from "lucide-react";
import { useT } from "../i18n";
import { baseName, CLIP_TONE, clipCategory, clipFilter, linkParts, parentFolder, shortAge, type ClipCategory } from "../fileKinds";
import { FileGlyph } from "./FileArt";
import { HScrollArrows, useHScroll } from "./HScroll";
import "./ShelfClipboard.css";

export type ClipItem = {
  id: number;
  kind: "text" | "image" | "files";
  preview: string;
  chars: number;
  at: number;
  width: number;
  height: number;
  thumb: string | null;
  /** First few paths of copied files, and how many there were. */
  files: string[];
  file_count: number;
  /** App the item was copied from, "" when unknown. */
  source: string;
};

type Filter = "all" | "text" | "link" | "image" | "file";
const FILTERS: Filter[] = ["all", "text", "link", "image", "file"];
const FILTER_LABEL: Record<Filter, string> = {
  all: "clipboard.filter.all",
  text: "clipboard.filter.text",
  link: "clipboard.filter.links",
  image: "clipboard.filter.images",
  file: "clipboard.filter.files",
};

const withDefaults = (item: ClipItem): ClipItem => ({ ...item, files: item.files ?? [], file_count: item.file_count ?? 0, source: item.source ?? "" });

// Increases with every call so the backend can drop one that arrives after a newer one.
let configureGeneration = Date.now();

export function useClipboardHistory(owner: boolean, enabled: boolean, listening: boolean, size: number, persist: boolean) {
  const [items, setItems] = useState<ClipItem[]>([]);

  // Only one notch window drives the shared listener, or several would switch it on and off over each other.
  useEffect(() => {
    if (!owner) return;
    invoke("clipboard_history_configure", { generation: ++configureGeneration, enabled: listening, size: Math.min(100, Math.max(5, Math.round(size) || 20)), persist }).catch(() => { });
  }, [owner, listening, size, persist]);

  useEffect(() => {
    if (!enabled) {
      setItems([]);
      return;
    }
    invoke<ClipItem[]>("clipboard_history_get").then((list) => setItems(list.map(withDefaults))).catch(() => { });
    const unlisten = listen<ClipItem[]>("clipboard-history-changed", (e) => setItems(e.payload.map(withDefaults)));
    return () => { unlisten.then((fn) => fn()); };
  }, [enabled]);

  const copy = useCallback((id: number) => invoke("clipboard_history_copy", { id }), []);
  const remove = useCallback((id: number) => { invoke("clipboard_history_delete", { id }).catch(() => { }); }, []);
  const clear = useCallback(() => { invoke("clipboard_history_clear").catch(() => { }); }, []);

  return { items, copy, remove, clear };
}

type Props = {
  items: ClipItem[];
  onCopy: (id: number) => Promise<unknown>;
  onRemove: (id: number) => void;
  onClear: () => void;
};

const CARD_SPRING = { type: "spring", stiffness: 520, damping: 36 } as const;

const oneLine = (text: string) => text.replace(/\s+/g, " ").trim();

const TYPE_ICON: Record<ClipCategory, typeof Type> = {
  text: Type,
  link: Link2,
  image: Image,
  file: File,
  files: Files,
};

function CardBody({ item, category }: { item: ClipItem; category: ClipCategory }) {
  const t = useT();
  if (category === "image") {
    return item.thumb ? <img className="clip-card-img" src={item.thumb} alt="" draggable={false} /> : <span className="clip-card-img empty" />;
  }
  if (category === "link") {
    const { host, rest } = linkParts(item.preview);
    return (
      <div className="clip-card-link">
        <span className="clip-card-host">{host}</span>
        {rest && <span className="clip-card-path">{rest}</span>}
      </div>
    );
  }
  if (category === "file") {
    const name = baseName(item.files[0] ?? "");
    return (
      <div className="clip-card-file">
        <FileGlyph name={name} />
        <span className="clip-card-filename">{name}</span>
      </div>
    );
  }
  if (category === "files") {
    return (
      <div className="clip-card-file">
        <span className="clip-card-stack">
          {item.files.slice(0, 3).map((path, i) => <FileGlyph key={`${i}-${path}`} name={baseName(path)} />)}
        </span>
        <span className="clip-card-count">{t("clipboard.fileCount", { count: item.file_count })}</span>
        <span className="clip-card-sub">{baseName(item.files[0] ?? "")}</span>
      </div>
    );
  }
  return <p className="clip-card-text">{item.preview.trim()}</p>;
}

function footerText(item: ClipItem, category: ClipCategory, t: ReturnType<typeof useT>) {
  if (category === "image") return `${item.width} × ${item.height}`;
  if (category === "file" || category === "files") return parentFolder(item.files[0] ?? "") || item.source;
  return item.source || (item.chars === 1 ? t("clipboard.charsOne") : t("clipboard.chars", { count: item.chars }));
}

/** What a card holds, short enough to tell cards of the same type apart when read out. */
function summaryText(item: ClipItem, category: ClipCategory, footer: string, max: number) {
  if (category === "image") return footer;
  if (category === "text" || category === "link") return oneLine(item.preview).slice(0, max).trimEnd();
  return item.files.map(baseName).join(", ").slice(0, max).trimEnd();
}

export function NotchClipboard({ items, onCopy, onRemove, onClear }: Props) {
  const t = useT();
  const [copiedId, setCopiedId] = useState<number | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 30000);
    return () => window.clearInterval(id);
  }, []);
  // A text whose preview is only blank space would draw an empty card.
  const visible = items.filter((item) => item.kind !== "text" || item.preview.trim() !== "");
  const withCategory = visible.map((item) => ({ item, category: clipCategory(item) }));
  const shown = filter === "all" ? withCategory : withCategory.filter(({ category }) => clipFilter(category) === filter);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const rowRef = useRef<HTMLDivElement>(null);
  const scroll = useHScroll(rowRef, [shown.length, filter]);

  useEffect(() => {
    rowRef.current?.scrollTo({ left: 0 });
  }, [filter]);

  const activate = (id: number) => {
    onCopy(id).then(() => {
      setCopiedId(id);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopiedId((cur) => (cur === id ? null : cur)), 1200);
    }).catch(() => { });
  };

  // With a filter on, Clear only removes what that filter shows.
  const clearShown = () => {
    if (filter === "all") onClear();
    else shown.forEach(({ item }) => onRemove(item.id));
  };
  const clearLabel = filter === "all" ? t("clipboard.clear") : `${t("clipboard.clear")}: ${t(FILTER_LABEL[filter])}`;

  const age = (at: number) => {
    const a = shortAge(at, now);
    return a.unit === "now" ? t("clipboard.age.now") : t(`clipboard.age.${a.unit}`, { count: a.count });
  };

  return (
    <MotionConfig reducedMotion="user">
      <div className="clip-panel" onClick={(e) => e.stopPropagation()}>
        {visible.length === 0 ? (
          <div className="clip-empty">
            <ClipboardList size={15} strokeWidth={2} aria-hidden="true" />
            {t("clipboard.empty")}
          </div>
        ) : (
          <>
            <div className="clip-bar">
              <div className="clip-chips" role="toolbar" aria-label={t("clipboard.filterLabel")}>
                {FILTERS.map((f) => (
                  <button
                    key={f}
                    type="button"
                    className={`clip-chip${filter === f ? " active" : ""}`}
                    aria-pressed={filter === f}
                    onClick={() => setFilter(f)}
                  >
                    {t(FILTER_LABEL[f])}
                  </button>
                ))}
              </div>
              <button type="button" className="clip-clear" aria-label={clearLabel} title={clearLabel} disabled={shown.length === 0} onClick={clearShown}>
                {t("clipboard.clear")}
              </button>
            </div>
            <div className={`clip-scroll${scroll.prev ? " fade-start" : ""}${scroll.next ? " fade-end" : ""}`}>
              <div ref={rowRef} className="clip-row" role="list" aria-label={t("panel.clipboard")}>
                <AnimatePresence initial={false} mode="popLayout">
                  {shown.map(({ item, category }) => {
                    const copied = copiedId === item.id;
                    const Icon = copied ? Check : TYPE_ICON[category];
                    const typeLabel = t(`clipboard.type.${category}`);
                    const footer = footerText(item, category, t);
                    return (
                      <motion.div
                        key={item.id}
                        layout="position"
                        role="listitem"
                        className={`clip-card ${category}${copied ? " copied" : ""}`}
                        style={{ "--clip-tone": CLIP_TONE[category] } as CSSProperties}
                        initial={{ opacity: 0, scale: 0.92 }}
                        animate={{ opacity: 1, scale: 1 }}
                        exit={{ opacity: 0, scale: 0.92, transition: { duration: 0.1 } }}
                        transition={CARD_SPRING}
                      >
                        <button
                          type="button"
                          className="clip-card-main"
                          title={category === "text" || category === "link" ? item.preview : category === "image" ? undefined : item.files.join("\n")}
                          aria-label={`${typeLabel}, ${age(item.at)}: ${summaryText(item, category, footer, 80)}`}
                          onClick={() => activate(item.id)}
                        >
                          <span className="clip-card-head">
                            <Icon size={13} strokeWidth={2.3} aria-hidden="true" />
                            <span className="clip-card-type">{copied ? t("clipboard.copied") : typeLabel}</span>
                            <span className="clip-card-age">{age(item.at)}</span>
                          </span>
                          <span className="clip-card-body">
                            <CardBody item={item} category={category} />
                          </span>
                          {footer && <span className="clip-card-foot">{footer}</span>}
                        </button>
                        <button
                          type="button"
                          className="clip-card-remove"
                          title={t("clipboard.remove")}
                          aria-label={`${t("clipboard.remove")}: ${summaryText(item, category, footer, 40) || typeLabel}`}
                          onClick={(e) => { e.stopPropagation(); onRemove(item.id); }}
                        >
                          <X size={12} strokeWidth={2.6} aria-hidden="true" />
                        </button>
                      </motion.div>
                    );
                  })}
                </AnimatePresence>
              </div>
              {shown.length === 0 && <div className="clip-none">{t("clipboard.none")}</div>}
              <HScrollArrows {...scroll} labels={{ prev: t("panel.scrollPrev"), next: t("panel.scrollNext") }} />
            </div>
            <span className="visually-hidden" aria-live="polite">{copiedId !== null ? t("clipboard.copied") : ""}</span>
          </>
        )}
      </div>
    </MotionConfig>
  );
}
