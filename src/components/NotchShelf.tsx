import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { startDrag } from "@crabnebula/tauri-plugin-drag";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, MotionConfig, Reorder } from "framer-motion";
import { ArrowDownToLine, Copy, Download, Smartphone, Trash2, X } from "lucide-react";
import { useT } from "../i18n";
import { PHONE_LINK } from "../buildFlags";
import { truncateName } from "../fileKinds";
import { FileArt } from "./FileArt";
import { HScrollArrows, useHScroll } from "./HScroll";
import "./ShelfClipboard.css";

export type ShelfItem = {
  id: string;
  kind: "file" | "text";
  path?: string;
  name: string;
  text?: string;
  thumb?: string | null;
  preview?: string | null;
  /** Folder or file, once the filesystem has said; unknown for older saved items until then. */
  dir?: boolean;
};

const STORAGE_KEY = "nectar-shelf-items";
const MAX_ITEMS = 10;

const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;
// Phone files that may get a shell thumbnail: images, video and audio only.
const PHONE_THUMB = /\.(png|jpe?g|gif|webp|bmp|heic|heif|avif|tiff?|mp4|m4v|mov|webm|mkv|avi|3gp|mp3|m4a|aac|wav|flac|ogg|opus)$/i;

function readSaved(): ShelfItem[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) || "[]");
    return Array.isArray(parsed) ? parsed.map((i: ShelfItem) => ({ ...i, thumb: null, preview: null })) : [];
  } catch {
    return [];
  }
}

type ShelfCallbacks = {
  onDragEnter: () => void;
  onDragLeave: () => void;
  onDrop: () => void;
};

type PhoneReceived = { seq?: number; paths?: string[]; text?: string };

export function useShelf(windowLabel: string, enabled: boolean, persist: boolean, cb: ShelfCallbacks) {
  const [items, setItems] = useState<ShelfItem[]>(() => (persist ? readSaved() : []));
  const [dropActive, setDropActive] = useState(false);
  const cbRef = useRef(cb);
  cbRef.current = cb;

  useEffect(() => {
    if (!persist) {
      try { localStorage.removeItem(STORAGE_KEY); } catch { /* storage unavailable */ }
      return;
    }
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(items.map(({ thumb, preview, ...rest }) => rest))); } catch { /* storage unavailable */ }
  }, [items, persist]);

  useEffect(() => {
    if (!persist) return;
    const files = items.filter((i) => i.kind === "file" && i.path).map((i) => i.path as string);
    if (files.length === 0) return;
    invoke<string[]>("shelf_prune", { paths: files }).then((alive) => {
      const keep = new Set(alive);
      setItems((prev) => prev.filter((i) => i.kind !== "file" || keep.has(i.path as string)));
    }).catch(() => { });
  }, []);

  // Explorer has no cached thumbnail for a fresh file yet (videos especially), so ask again a few times.
  // Files from the phone (ids "p-...") reach the shell thumbnail code only for media types (I-153).
  const loadThumb = useCallback((id: string, path: string, attempt = 0) => {
    const preview = () => invoke<string | null>("shelf_text_preview", { path }).then((preview) => {
      if (preview) {
        setItems((prev) => prev.map((i) => (i.id === id ? { ...i, preview } : i)));
      } else if (attempt < 3) {
        setTimeout(() => loadThumb(id, path, attempt + 1), 600 * (attempt + 1));
      }
    }).catch(() => { });
    if (id.startsWith("p-") && !PHONE_THUMB.test(path)) {
      preview();
      return;
    }
    invoke<string | null>("shelf_thumbnail", { path }).then((thumb) => {
      if (thumb) {
        setItems((prev) => prev.map((i) => (i.id === id ? { ...i, thumb } : i)));
        return;
      }
      preview();
    }).catch(() => { });
  }, []);

  // A name alone cannot tell a folder from a file without an extension (.gitignore, LICENSE).
  const loadDirs = useCallback((list: ShelfItem[]) => {
    const unknown = list.filter((i) => i.kind === "file" && i.path && i.dir === undefined);
    if (unknown.length === 0) return;
    invoke<boolean[]>("shelf_dirs", { paths: unknown.map((i) => i.path) }).then((dirs) => {
      const byId = new Map(unknown.map((i, n) => [i.id, dirs[n]]));
      setItems((prev) => prev.map((i) => (typeof byId.get(i.id) === "boolean" ? { ...i, dir: byId.get(i.id) } : i)));
    }).catch(() => { });
  }, []);

  useEffect(() => {
    items.forEach((i) => { if (i.kind === "file" && i.path && !i.thumb && !i.preview) loadThumb(i.id, i.path); });
    loadDirs(items);
  }, []);

  // `key` marks an item from the phone, which can arrive both as an event and from the queue.
  const add = useCallback((paths?: string[], text?: string, key?: string) => {
    const fresh: ShelfItem[] = [];
    if (paths) {
      for (const path of paths) {
        // The phone only ever sends files.
        fresh.push({ id: key ? `${key}-${fresh.length}` : `f-${Date.now()}-${fresh.length}-${path}`, kind: "file", path, name: baseName(path), ...(key ? { dir: false } : {}) });
      }
    } else if (text) {
      fresh.push({ id: key ?? `t-${Date.now()}`, kind: "text", name: text.trim().slice(0, 40), text });
    }
    if (fresh.length === 0) return;
    let added = fresh;
    setItems((prev) => {
      added = fresh.filter((i) => !prev.some((p) => p.id === i.id));
      const taken = new Set(added.filter((i) => i.path).map((i) => i.path));
      const rest = prev.filter((i) => !(i.path && taken.has(i.path)));
      return [...added, ...rest].slice(0, MAX_ITEMS);
    });
    added.forEach((i) => { if (i.path) loadThumb(i.id, i.path); });
    loadDirs(added);
  }, [loadThumb, loadDirs]);

  useEffect(() => {
    if (!enabled || !windowLabel) return;
    const isNotch = windowLabel === "main" || windowLabel.startsWith("main-");
    if (!isNotch) return;
    invoke("setup_shelf_drop").catch(() => { });
    const addReceived = (r: PhoneReceived) => {
      const key = typeof r.seq === "number" ? `p-${r.seq}` : undefined;
      if (r.paths?.length) add(r.paths, undefined, key);
      else if (r.text) add(undefined, r.text, key);
    };
    // Anything that came while no notch was listening (the first mailbox scan at startup).
    const takeReceived = PHONE_LINK
      ? () => { invoke<PhoneReceived[]>("phone_link_take_received").then((list) => list.forEach(addReceived)).catch(() => { }); }
      : () => { };
    takeReceived();

    const unlistens: Promise<() => void>[] = [
      listen<{ label: string }>("shelf-drag-enter", (e) => {
        if (e.payload.label === windowLabel) {
          setDropActive(true);
          cbRef.current.onDragEnter();
        }
      }),
      listen<{ label: string }>("shelf-drag-leave", (e) => {
        if (e.payload.label === windowLabel) {
          setDropActive(false);
          cbRef.current.onDragLeave();
        }
      }),
      listen<{ label: string; paths?: string[]; text?: string }>("shelf-drag-drop", (e) => {
        add(e.payload.paths, e.payload.text);
        setDropActive(false);
        if (e.payload.label === windowLabel) cbRef.current.onDrop();
      }),
      listen<{ path: string }>("download-finished", (e) => add([e.payload.path])),
    ];
    if (PHONE_LINK) {
      // Files and text from a paired phone land on the shelf like a drop (design 7.2).
      unlistens.push(listen<PhoneReceived>("phone-link-received", (e) => {
        addReceived(e.payload);
        takeReceived();
      }));
    }
    return () => { unlistens.forEach((u) => u.then((fn) => fn())); };
  }, [enabled, windowLabel, add]);

  const remove = useCallback((id: string) => setItems((prev) => prev.filter((i) => i.id !== id)), []);
  const clear = useCallback(() => setItems([]), []);

  return { items, dropActive, reorder: setItems, remove, clear };
}

type Props = {
  items: ShelfItem[];
  dropActive: boolean;
  onReorder: (items: ShelfItem[]) => void;
  onRemove: (id: string) => void;
  onClear: () => void;
  /** Also used to keep the notch open while a folder picker is up. */
  onDragOutStart: () => void;
  onDragOutEnd: () => void;
  header?: ReactNode;
};

const CARD_SPRING = { type: "spring", stiffness: 520, damping: 34 } as const;

type PhoneTarget = { id: string; name: string; files: boolean; text: boolean; online: boolean };

const NO_TARGETS: PhoneTarget[] = [];

/** Paired phones that may receive from this PC; empty while the phone link is off. */
const usePhoneTargets = PHONE_LINK ? usePairedPhones : () => NO_TARGETS;

function usePairedPhones() {
  const [targets, setTargets] = useState<PhoneTarget[]>([]);
  useEffect(() => {
    const load = () => { invoke<PhoneTarget[]>("phone_link_targets").then(setTargets).catch(() => setTargets([])); };
    load();
    const subs = ["phone-link-devices", "phone-link-session", "phone-link-state"].map((ev) => listen(ev, load));
    return () => { subs.forEach((u) => u.then((fn) => fn())); };
  }, []);
  return targets;
}

const NAME_CHARS = 13;

export function NotchShelf({ items, dropActive, onReorder, onRemove, onClear, onDragOutStart, onDragOutEnd, header }: Props) {
  const t = useT();
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [phoneNote, setPhoneNote] = useState<{ id: string; text: string } | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const statusTimer = useRef<number | undefined>(undefined);
  const targets = usePhoneTargets();
  const selected = items.find((i) => i.id === selectedId) ?? null;

  useEffect(() => () => window.clearTimeout(statusTimer.current), []);
  useEffect(() => {
    if (selectedId && !items.some((i) => i.id === selectedId)) setSelectedId(null);
  }, [items, selectedId]);

  const flash = (text: string) => {
    setStatus(text);
    window.clearTimeout(statusTimer.current);
    statusTimer.current = window.setTimeout(() => setStatus(null), 1800);
  };

  const targetFor = (item: ShelfItem) => {
    const usable = targets.filter((p) => (item.kind === "file" ? p.files : p.text));
    return usable.find((p) => p.online) ?? usable[0];
  };

  const sendOne = PHONE_LINK
    ? async (item: ShelfItem): Promise<"sent" | "queued" | "failed"> => {
      const target = targetFor(item);
      if (!target) return "failed";
      const args = item.kind === "file" ? { deviceId: target.id, path: item.path } : { deviceId: target.id, text: item.text };
      try {
        const r = await invoke<{ queued?: boolean }>("phone_link_send", args);
        return r?.queued ? "queued" : "sent";
      } catch {
        return "failed";
      }
    }
    : async (_item: ShelfItem): Promise<"sent" | "queued" | "failed"> => "failed";

  const noteFor = (result: "sent" | "queued" | "failed") =>
    result === "queued" ? t("shelf.queuedForPhone") : result === "sent" ? t("shelf.sentToPhone") : t("shelf.sendFailed");

  const sendToPhone = async (item: ShelfItem) => {
    const text = noteFor(await sendOne(item));
    setPhoneNote({ id: item.id, text });
    setTimeout(() => setPhoneNote((cur) => (cur?.id === item.id ? null : cur)), 1600);
  };

  const sendable = (selected ? [selected] : items).filter((i) => targetFor(i));
  const sendSelectionToPhone = async () => {
    if (sendable.length === 0) return;
    const results = await Promise.all(sendable.map(sendOne));
    flash(noteFor(results.includes("failed") ? "failed" : results.includes("queued") ? "queued" : "sent"));
  };

  const scope = selected ? [selected] : items;

  const saveToFolder = async () => {
    onDragOutStart();
    try {
      const dir = await openDialog({ directory: true, multiple: false, title: t("shelf.saveTitle") });
      if (typeof dir !== "string") return;
      const paths = scope.filter((i) => i.kind === "file" && i.path).map((i) => i.path as string);
      const texts = scope.filter((i) => i.kind === "text" && i.text).map((i) => i.text as string);
      const saved = await invoke<number>("shelf_save_to", { dir, paths, texts });
      flash(saved > 0 ? t("shelf.saved", { count: saved }) : t("shelf.saveFailed"));
      if (saved > 0) invoke("shelf_open", { path: dir }).catch(() => { });
    } catch {
      flash(t("shelf.saveFailed"));
    } finally {
      onDragOutEnd();
    }
  };

  const copyToClipboard = async () => {
    try {
      const files = scope.filter((i) => i.kind === "file" && i.path).map((i) => i.path as string);
      const texts = scope.filter((i) => i.kind === "text" && i.text).map((i) => i.text as string);
      if (files.length === 0) {
        await navigator.clipboard.writeText(texts.join("\n\n"));
      } else {
        const notes = await Promise.all(texts.map((text) => invoke<string | null>("shelf_text_file", { text })));
        await invoke("clipboard_copy_files", { paths: [...files, ...notes.filter((p): p is string => !!p)] });
      }
      flash(t("shelf.copied"));
    } catch {
      flash(t("shelf.copyFailed"));
    }
  };

  const [resetKey, setResetKey] = useState(0);
  const panelRef = useRef<HTMLDivElement>(null);
  const rowRef = useRef<HTMLDivElement>(null);
  const dragState = useRef<{ out: boolean } | null>(null);
  const dragIcon = useRef<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const scroll = useHScroll(rowRef, [items.length, resetKey]);

  useEffect(() => {
    invoke<string | null>("shelf_drag_icon").then((icon) => { dragIcon.current = icon; }).catch(() => { });
  }, []);

  const beginDrag = async (item: ShelfItem) => {
    const icon = item.thumb ?? dragIcon.current;
    if (!icon) return;
    const path = item.kind === "file" ? item.path : await invoke<string | null>("shelf_text_file", { text: item.text ?? "" });
    if (!path) return;
    onDragOutStart();
    try {
      await startDrag({ item: [path], icon, mode: "copy" });
    } catch { /* drag was cancelled or refused */ }
    setDragging(false);
    setResetKey((k) => k + 1);
    onDragOutEnd();
  };

  const activate = (item: ShelfItem) => {
    if (item.kind === "file" && item.path) {
      invoke("shelf_open", { path: item.path }).catch(() => { });
    } else if (item.text) {
      navigator.clipboard.writeText(item.text).then(() => {
        setCopiedId(item.id);
        setTimeout(() => setCopiedId((cur) => (cur === item.id ? null : cur)), 1200);
      }).catch(() => { });
    }
  };

  const toggleSelect = (id: string) => setSelectedId((cur) => (cur === id ? null : id));
  const count = items.length;
  // Up to four tiles fit the narrower shelf widths (App's shelf width rule); the measurement guards the rest.
  const fits = count <= 4 && !scroll.prev && !scroll.next;

  return (
    <MotionConfig reducedMotion="user">
      <div
        ref={panelRef}
        className={`shelf-panel${dropActive ? " drop-active" : ""}`}
        onClick={(e) => { e.stopPropagation(); setSelectedId(null); }}
      >
        {header}
        {count === 0 ? (
          <div className="shelf-empty">
            <ArrowDownToLine size={15} strokeWidth={2} aria-hidden="true" />
            {t("shelf.empty")}
          </div>
        ) : (
          <>
            <div className="shelf-head">
              <h2 className="shelf-title">
                {t("panel.shelf")}
                <span className="shelf-meta" aria-live="polite">
                  {status ?? (selected ? t("shelf.selectedOne") : count === 1 ? t("shelf.countOne") : t("shelf.countMany", { count }))}
                </span>
              </h2>
              <div className="shelf-actions">
                {sendable.length > 0 && (
                  <button
                    type="button"
                    className="shelf-pill"
                    title={selected ? `${t("shelf.sendToPhone")}: ${selected.name}` : t("shelf.sendAllToPhone")}
                    onClick={(e) => { e.stopPropagation(); void sendSelectionToPhone(); }}
                  >
                    <Smartphone size={13} strokeWidth={2.2} aria-hidden="true" />
                    {t("shelf.sendToPhone")}
                  </button>
                )}
                <button type="button" className="shelf-icon-btn" aria-label={selected ? t("shelf.saveOne") : t("shelf.saveAll")} title={selected ? t("shelf.saveOne") : t("shelf.saveAll")} onClick={(e) => { e.stopPropagation(); void saveToFolder(); }}>
                  <Download size={15} strokeWidth={2} aria-hidden="true" />
                </button>
                <button type="button" className="shelf-icon-btn" aria-label={selected ? t("shelf.copyOne") : t("shelf.copyAll")} title={selected ? t("shelf.copyOne") : t("shelf.copyAll")} onClick={(e) => { e.stopPropagation(); void copyToClipboard(); }}>
                  <Copy size={14} strokeWidth={2} aria-hidden="true" />
                </button>
                <button type="button" className="shelf-icon-btn" aria-label={t("shelf.clearAll")} title={t("shelf.clearAll")} onClick={(e) => { e.stopPropagation(); onClear(); }}>
                  <Trash2 size={14} strokeWidth={2} aria-hidden="true" />
                </button>
              </div>
            </div>
            <div className={`shelf-scroll${scroll.prev ? " fade-start" : ""}${scroll.next ? " fade-end" : ""}${fits ? " fits" : ""}${dragging ? " dragging" : ""}`}>
              <Reorder.Group
                ref={rowRef}
                key={resetKey}
                as="div"
                axis="x"
                layoutScroll
                values={items}
                onReorder={onReorder}
                className="shelf-row"
                role="list"
                aria-label={t("panel.shelf")}
              >
                <AnimatePresence initial={false}>
                  {items.map((item) => {
                    const label = phoneNote?.id === item.id ? phoneNote.text : copiedId === item.id ? t("shelf.copied") : item.kind === "file" ? truncateName(item.name, NAME_CHARS) : item.name;
                    const isSelected = item.id === selectedId;
                    return (
                      <Reorder.Item
                        key={item.id}
                        as="div"
                        value={item}
                        role="listitem"
                        // onTap would make the tile a tab stop of its own; the button inside is the keyboard entry.
                        tabIndex={-1}
                        className={`shelf-tile ${item.kind}${isSelected ? " selected" : ""}`}
                        title={item.kind === "text" ? item.text : item.path}
                        initial={{ opacity: 0, scale: 0.6, y: -8 }}
                        animate={{ opacity: 1, scale: 1, y: 0 }}
                        exit={{ opacity: 0, scale: 0.6, transition: { duration: 0.12 } }}
                        whileDrag={{ scale: 1.08, zIndex: 5 }}
                        transition={CARD_SPRING}
                        onDragStart={() => { dragState.current = { out: false }; setDragging(true); }}
                        onDrag={(_, info) => {
                          const state = dragState.current;
                          const rect = panelRef.current?.getBoundingClientRect();
                          if (!state || state.out || !rect) return;
                          const { x, y } = info.point;
                          if (x < rect.left || x > rect.right || y > rect.bottom || y < rect.top - 20) {
                            state.out = true;
                            beginDrag(item);
                          }
                        }}
                        onDragEnd={() => { dragState.current = null; setDragging(false); }}
                        onTap={(e) => {
                          // A tap on a note copies it and leaves the selection alone; Ctrl+tap selects it.
                          if (item.kind === "text" && !e.ctrlKey && !e.metaKey) activate(item);
                          else toggleSelect(item.id);
                        }}
                        onDoubleClick={() => { if (item.kind === "file") activate(item); }}
                        onClick={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          className="shelf-tile-main"
                          aria-pressed={isSelected}
                          aria-label={item.kind === "file" ? item.name : `${t("shelf.note")}: ${item.name}`}
                          onClick={(e) => {
                            // Pointer taps are handled by onTap (it skips drags); this is the keyboard path.
                            if (e.detail === 0) toggleSelect(item.id);
                          }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") { e.preventDefault(); activate(item); }
                            else if (e.key === "Delete") { e.preventDefault(); onRemove(item.id); }
                          }}
                        >
                          <FileArt name={item.name} thumb={item.thumb} preview={item.preview} dir={item.dir} note={item.kind === "text" ? (item.text ?? "") : null} />
                          <span className="shelf-name">{label}</span>
                        </button>
                        {targetFor(item) && (
                          <button
                            type="button"
                            className="shelf-chip send"
                            title={`${t("shelf.sendToPhone")}: ${targetFor(item)!.name}`}
                            aria-label={`${t("shelf.sendToPhone")}: ${item.name}`}
                            onPointerDown={(e) => e.stopPropagation()}
                            onClick={(e) => { e.stopPropagation(); void sendToPhone(item); }}
                          >
                            <Smartphone size={12} strokeWidth={2.4} aria-hidden="true" />
                          </button>
                        )}
                        <button
                          type="button"
                          className="shelf-chip remove"
                          title={t("shelf.remove")}
                          aria-label={`${t("shelf.remove")}: ${item.name}`}
                          onPointerDown={(e) => e.stopPropagation()}
                          onClick={(e) => { e.stopPropagation(); onRemove(item.id); }}
                        >
                          <X size={12} strokeWidth={2.6} aria-hidden="true" />
                        </button>
                      </Reorder.Item>
                    );
                  })}
                </AnimatePresence>
              </Reorder.Group>
              <HScrollArrows {...scroll} labels={{ prev: t("panel.scrollPrev"), next: t("panel.scrollNext") }} />
            </div>
          </>
        )}
      </div>
    </MotionConfig>
  );
}
