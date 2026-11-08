import { useEffect, useReducer, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createNoticeQueue, pickNotice, stackShownId, showDuration, AFTER_HOVER_MS, type NotchNotice } from "../notchNotifications";

interface Options {
  /** Something with a higher priority owns the notch (agent ask, fullscreen, a panel); the card waits. */
  blocked: boolean;
  /** Do Not Disturb in Nectar: new notices are dropped, like Windows does with banners. */
  quiet: boolean;
  /** The user has the notch open; a new card must not cover what they are using. */
  open: boolean;
  /** Cursor on the notch: keeps the card on screen. */
  hovered: boolean;
}

/** Windows toasts from the Rust listener (notifications.rs), one card at a time. No timers run while idle. */
export function useNotchNotifications(active: boolean, { blocked, quiet, open, hovered }: Options) {
  const queue = useRef(createNoticeQueue());
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const shownId = useRef<number | null>(null);
  const heldByHover = useRef(false);
  const quietRef = useRef(quiet);
  quietRef.current = quiet;

  useEffect(() => {
    if (!active) {
      queue.current.clear();
      bump();
      return;
    }
    const unlisteners = [
      listen<NotchNotice>("notch-notification", (e) => {
        if (quietRef.current || !e.payload) return;
        queue.current.push(e.payload);
        bump();
      }),
      listen<number[]>("notch-notification-removed", (e) => {
        let changed = false;
        let onScreen = false;
        for (const id of e.payload ?? []) {
          const removed = queue.current.remove(id);
          changed = removed || changed;
          onScreen = onScreen || (removed && id === shownId.current);
        }
        // The card on screen went away in Windows: hand on to the next one like × does.
        if (onScreen) shownId.current = stackShownId(queue.current, shownId.current, true);
        if (changed) bump();
      }),
      listen("notch-notifications-clear", () => {
        queue.current.clear();
        bump();
      }),
    ];
    return () => unlisteners.forEach((u) => u.then((fn) => fn()));
  }, [active]);

  const notice = pickNotice(queue.current, shownId.current, { blocked, open, now: Date.now() });
  if (notice) shownId.current = notice.id;
  const waiting = queue.current.waiting();

  const advance = (byUser: boolean) => {
    heldByHover.current = false;
    queue.current.advance();
    shownId.current = stackShownId(queue.current, shownId.current, byUser);
    bump();
  };
  const next = () => advance(true);

  useEffect(() => {
    if (!notice) return;
    if (hovered) {
      heldByHover.current = true;
      return;
    }
    const ms = heldByHover.current ? AFTER_HOVER_MS : showDuration(queue.current.waiting());
    const id = setTimeout(() => advance(false), ms);
    return () => clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [notice?.id, hovered]);

  const dismiss = () => {
    if (!notice) return;
    invoke("notch_notification_dismiss", { id: notice.id }).catch(() => { });
    next();
  };

  const openApp = () => {
    if (!notice) return;
    invoke("notch_notification_open", { aumid: notice.aumid, title: notice.title }).catch(() => { });
    next();
  };

  return { notice, waiting, next, dismiss, open: openApp };
}
