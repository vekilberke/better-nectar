// Mic/camera in-use indicator and meeting mode (T-019). The state comes from privacy.rs (`privacy-state`).

export interface PrivacyState {
  mic: string[];
  camera: string[];
  /** Red mic: every microphone muted, or Nectar's mute still holds (also after meeting mode was turned off). */
  muted: boolean;
  /** Nectar muted, but a microphone is live again; the mic turns amber and a click still unmutes the rest. */
  partial: boolean;
  meeting: boolean;
}

export const EMPTY_PRIVACY: PrivacyState = { mic: [], camera: [], muted: false, partial: false, meeting: false };

const names = (v: unknown): string[] => {
  if (!Array.isArray(v)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const n of v) {
    if (typeof n !== "string") continue;
    const name = n.trim();
    if (!name || seen.has(name.toLowerCase())) continue;
    seen.add(name.toLowerCase());
    out.push(name);
  }
  return out;
};

/** Event payloads and the startup snapshot both pass through here; anything malformed reads as "nothing in use". */
export function normalizePrivacy(raw: unknown): PrivacyState {
  if (!raw || typeof raw !== "object") return EMPTY_PRIVACY;
  const r = raw as Record<string, unknown>;
  const muted = r.muted === true;
  return { mic: names(r.mic), camera: names(r.camera), muted, partial: muted && r.partial === true, meeting: r.meeting === true };
}

export function samePrivacy(a: PrivacyState, b: PrivacyState): boolean {
  return a.muted === b.muted && a.partial === b.partial && a.meeting === b.meeting && a.mic.join("\n") === b.mic.join("\n") && a.camera.join("\n") === b.camera.join("\n");
}

export type PrivacyItem = "muted" | "mic" | "camera";

/** What the notch shows, left to right: the red muted mic, then the orange mic dot, then the green camera dot. */
export function privacyItems(s: PrivacyState): PrivacyItem[] {
  const items: PrivacyItem[] = [];
  if (s.muted) items.push("muted");
  if (s.mic.length) items.push("mic");
  if (s.camera.length) items.push("camera");
  return items;
}

// Keep in step with .privacy-lane / .privacy-chip in App.css.
export const PRIVACY_DOT = 6;
export const PRIVACY_ICON = 13;
export const PRIVACY_GAP = 4;
/** Space between the collapsed notch's own content and the lane. */
export const PRIVACY_LEAD = 6;
export const PRIVACY_CHIP_PAD = 5;
/** Gap between the tab strip's last icon and the chip. */
const CHIP_SPACE = 6;

function itemsWidth(items: readonly PrivacyItem[]): number {
  if (!items.length) return 0;
  return items.reduce((w, i) => w + (i === "muted" ? PRIVACY_ICON : PRIVACY_DOT), 0) + PRIVACY_GAP * (items.length - 1);
}

/** Width the collapsed notch grows by; the lane sits on its right, so clock, cat and agent bar keep their own widths. */
export function privacyLaneWidth(items: readonly PrivacyItem[]): number {
  return items.length ? PRIVACY_LEAD + itemsWidth(items) : 0;
}

export function privacyChipWidth(items: readonly PrivacyItem[]): number {
  return items.length ? itemsWidth(items) + PRIVACY_CHIP_PAD * 2 : 0;
}

/**
 * Extra width the open notch needs for the chip at the tab strip's right edge. The tabs stay
 * centred, so the chip's room is kept on both sides.
 */
export function tabStripReserve(items: readonly PrivacyItem[]): number {
  return items.length ? 2 * (privacyChipWidth(items) + CHIP_SPACE) : 0;
}

/** "Zoom, Google Chrome +2" */
export function formatApps(list: readonly string[], max = 2): string {
  if (list.length <= max) return list.join(", ");
  return `${list.slice(0, max).join(", ")} +${list.length - max}`;
}

export interface PrivacyLabels {
  mic: string;
  camera: string;
  muted: string;
  partial: string;
}

/** One line per thing in use, e.g. "Microphone: Zoom, Teams" / "Camera: Zoom" / "Microphone muted". */
export function privacyLines(s: PrivacyState, labels: PrivacyLabels, max = 2): string[] {
  const lines: string[] = [];
  if (s.muted) lines.push(s.partial ? labels.partial : labels.muted);
  if (s.mic.length) lines.push(`${labels.mic}: ${formatApps(s.mic, max)}`);
  if (s.camera.length) lines.push(`${labels.camera}: ${formatApps(s.camera, max)}`);
  return lines;
}
