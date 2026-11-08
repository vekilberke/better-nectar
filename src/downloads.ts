/** One entry of `downloads_list` / `downloads-changed` (Rust `downloads::DownloadView`). */
export type Download = {
  id: number;
  name: string;
  bytes: number;
  speed: number;
  done: boolean;
};

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 1024-based, one decimal under 100 (`812 KB`, `4.2 MB`, `1.3 GB`). */
export function formatBytes(bytes: number): string {
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  // 1048500 B is 1023.9 KB, which would round to "1024 KB": show "1 MB" instead.
  if (unit > 0 && Math.round(value) >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  const text = unit === 0 || value >= 100 ? String(Math.round(value)) : value.toFixed(1).replace(/\.0$/, "");
  return `${text} ${UNITS[unit]}`;
}

export function formatSpeed(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

/** What the collapsed notch shows: a spinning ring while anything runs, a check when only finished ones are left. */
export function downloadsSummary(list: Download[]): { active: number; done: number; speed: number } {
  let active = 0;
  let done = 0;
  let speed = 0;
  for (const d of list) {
    if (d.done) done++;
    else {
      active++;
      speed += d.speed;
    }
  }
  return { active, done, speed };
}

export const DOWNLOAD_ROW_H = 34;
export const DOWNLOAD_ROWS_MAX = 3;

/** Rows shown at the top of the Shelf panel (the rest is summed up in a "+N" line). */
export function visibleDownloads(list: Download[]): { rows: Download[]; more: number } {
  const ordered = [...list.filter((d) => !d.done), ...list.filter((d) => d.done)];
  return { rows: ordered.slice(0, DOWNLOAD_ROWS_MAX), more: Math.max(0, ordered.length - DOWNLOAD_ROWS_MAX) };
}

/** Extra Shelf panel height for the downloads list (0 when there is none). */
export function downloadsListHeight(list: Download[]): number {
  if (list.length === 0) return 0;
  const { rows, more } = visibleDownloads(list);
  return rows.length * DOWNLOAD_ROW_H + (more ? 16 : 0) + 6;
}
