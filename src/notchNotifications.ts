// Windows toasts waiting for (or showing in) the notch. Kept in memory only.

export interface NotchNotice {
  id: number;
  app: string;
  aumid: string;
  title: string;
  body: string;
  icon?: string | null;
  time: number;
}

export const SHOW_MS = 5000;
// With more waiting behind it each card gets a little less time.
export const SHOW_MS_QUEUED = 4000;
// After the cursor leaves a card it was holding open.
export const AFTER_HOVER_MS = 2500;
export const MAX_QUEUE = 20;
// Cards held back (fullscreen, agent ask, open panel) longer than this are no longer news; Notification Center has them.
export const MAX_WAIT_MS = 3 * 60_000;

export function showDuration(waiting: number): number {
  return waiting > 0 ? SHOW_MS_QUEUED : SHOW_MS;
}

export function createNoticeQueue() {
  let items: NotchNotice[] = [];

  return {
    /** Adds a notice (or refreshes one with the same id). Returns false for a refresh. */
    push(n: NotchNotice): boolean {
      const at = items.findIndex((i) => i.id === n.id);
      if (at >= 0) {
        items[at] = n;
        return false;
      }
      items.push(n);
      // Too many waiting: drop the oldest waiting ones, never the one on screen.
      while (items.length > MAX_QUEUE) items.splice(1, 1);
      return true;
    },
    remove(id: number): boolean {
      const before = items.length;
      items = items.filter((i) => i.id !== id);
      return items.length !== before;
    },
    current(): NotchNotice | null {
      return items[0] ?? null;
    },
    waiting(): number {
      return Math.max(0, items.length - 1);
    },
    /** Drops notices older than MAX_WAIT_MS, except the one on screen. Returns true when any were dropped. */
    dropStale(now: number, keepId: number | null): boolean {
      const before = items.length;
      items = items.filter((i) => i.id === keepId || now - i.time <= MAX_WAIT_MS);
      return items.length !== before;
    },
    advance(): NotchNotice | null {
      items = items.slice(1);
      return items[0] ?? null;
    },
    clear() {
      items = [];
    },
    size(): number {
      return items.length;
    },
  };
}

export type NoticeQueue = ReturnType<typeof createNoticeQueue>;

const CHARS_PER_LINE = 50;

/** Body lines the card makes room for: 0 to 3, by length and line breaks. */
export function bodyLines(body: string): number {
  if (!body.trim()) return 0;
  const lines = body.split("\n").reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / CHARS_PER_LINE)), 0);
  return Math.min(3, lines);
}

/** A card may start only while the notch is closed; once on screen it stays until it times out or is dismissed. */
export function canShow(opts: { blocked: boolean; open: boolean; alreadyShowing: boolean }): boolean {
  if (opts.blocked) return false;
  return opts.alreadyShowing || !opts.open;
}

/**
 * The card to render now. `shownId` is the notice already on screen (or handed over by a user
 * action through the stack, see `stackShownId`); a new one only starts while the notch is closed.
 */
export function pickNotice(queue: NoticeQueue, shownId: number | null, opts: { blocked: boolean; open: boolean; now: number }): NotchNotice | null {
  // A card about to start after a wait: skip what went stale while the notch was busy.
  if (!opts.blocked && !opts.open && shownId !== queue.current()?.id) queue.dropStale(opts.now, shownId);
  const current = queue.current();
  return current && canShow({ blocked: opts.blocked, open: opts.open, alreadyShowing: shownId === current.id }) ? current : null;
}

/**
 * After the queue moved on. A user action on the card (+N, ×, click) keeps the stack on screen,
 * so the next card counts as already showing even with the cursor on the open notch. A timeout
 * does not: the next card waits for the notch to be free like any new one.
 */
export function stackShownId(queue: NoticeQueue, shownId: number | null, byUser: boolean): number | null {
  return byUser ? queue.current()?.id ?? null : shownId;
}
