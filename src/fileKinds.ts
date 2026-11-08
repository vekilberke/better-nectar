// File type → artwork and colour for the shelf and clipboard panels, plus the small text helpers they share.

export type FileKind =
  | "image"
  | "video"
  | "audio"
  | "pdf"
  | "doc"
  | "sheet"
  | "slides"
  | "archive"
  | "code"
  | "text"
  | "app"
  | "folder"
  | "generic";

/** How a kind is drawn: a photo card, a film strip, a record, a box, a folder or a page with a label. */
export type FileArtShape = "photo" | "film" | "record" | "box" | "folder" | "page";

const EXTENSIONS: Record<Exclude<FileKind, "folder" | "generic">, string[]> = {
  image: ["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif", "avif", "tif", "tiff", "svg", "ico", "raw", "psd"],
  video: ["mp4", "m4v", "mov", "webm", "mkv", "avi", "3gp", "wmv", "flv"],
  audio: ["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "wma", "aiff"],
  pdf: ["pdf"],
  doc: ["doc", "docx", "odt", "rtf", "pages"],
  sheet: ["xls", "xlsx", "csv", "tsv", "ods", "numbers"],
  slides: ["ppt", "pptx", "odp", "key"],
  archive: ["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso", "cab"],
  code: ["js", "jsx", "ts", "tsx", "rs", "py", "go", "java", "c", "h", "cpp", "cs", "html", "htm", "css", "json", "xml", "yaml", "yml", "toml", "sh", "ps1", "bat", "sql"],
  text: ["txt", "md", "markdown", "log", "ini", "cfg", "conf"],
  app: ["exe", "msi", "msix", "appx", "apk", "dmg", "lnk"],
};

const BY_EXT = new Map<string, FileKind>(
  (Object.entries(EXTENSIONS) as [FileKind, string[]][]).flatMap(([kind, exts]) => exts.map((e) => [e, kind] as const)),
);

const SHAPES: Record<FileKind, FileArtShape> = {
  image: "photo",
  video: "film",
  audio: "record",
  archive: "box",
  folder: "folder",
  pdf: "page",
  doc: "page",
  sheet: "page",
  slides: "page",
  code: "page",
  text: "page",
  app: "page",
  generic: "page",
};

/** Label colour for each kind, picked to stay readable on the dark notch. */
export const KIND_TONE: Record<FileKind, string> = {
  image: "#3fcf8e",
  video: "#4c8dff",
  audio: "#ff8a3d",
  pdf: "#ef4f4f",
  doc: "#3e8bff",
  sheet: "#26a95f",
  slides: "#f2780c",
  archive: "#c99a45",
  code: "#7f8ea3",
  text: "#9aa3ae",
  app: "#14b8a6",
  folder: "#f0b43c",
  generic: "#8e8e93",
};

/** Darker fill for the white type label on a page (PDF, DOCX...): at least 4.5:1 against white. */
export const KIND_BADGE: Record<FileKind, string> = {
  image: "#218458",
  video: "#2a63d6",
  audio: "#b84b06",
  pdf: "#d4393b",
  doc: "#2c6be0",
  sheet: "#1d8048",
  slides: "#b8530a",
  archive: "#85612a",
  code: "#5c6b82",
  text: "#636e7b",
  app: "#0d7a6e",
  folder: "#8f650f",
  generic: "#6b6b71",
};

/** Lower-case extension without the dot, "" when the name has none. */
export function fileExt(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "";
  const ext = name.slice(dot + 1);
  return ext.length <= 8 && !/[\s\\/]/.test(ext) ? ext.toLowerCase() : "";
}

/**
 * `dir` is what the filesystem said, when it was asked. Without it a name with no extension is taken for a
 * folder: that is what Explorer drops and copies most of the time.
 */
export function fileKind(name: string, dir?: boolean | null): FileKind {
  if (dir) return "folder";
  const ext = fileExt(name);
  if (!ext) return dir === false ? "generic" : "folder";
  return BY_EXT.get(ext) ?? "generic";
}

export const artShape = (kind: FileKind): FileArtShape => SHAPES[kind];

/** The short label printed on a page, e.g. PDF, DOCX, ZIP. */
export function kindLabel(name: string): string {
  return fileExt(name).toUpperCase().slice(0, 4) || "FILE";
}

/** Shortens a file name to about `max` characters but keeps its extension: "Sunset….png". Counts code points, so an emoji is never cut in half. */
export function truncateName(name: string, max: number): string {
  const chars = Array.from(name);
  if (chars.length <= max) return name;
  const dot = chars.lastIndexOf(".");
  const ext = dot > 0 && chars.length - dot <= 7 ? chars.slice(dot).join("") : "";
  const room = max - Array.from(ext).length - 1;
  if (room < 3) return `${chars.slice(0, Math.max(1, max - 1)).join("").trimEnd()}…`;
  return `${chars.slice(0, room).join("").trimEnd()}…${ext}`;
}

export const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/** The folder a path sits in, by name ("Temp" for C:\Users\me\AppData\Local\Temp\a.png). */
export function parentFolder(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.length >= 2 ? parts[parts.length - 2] : "";
}

export type ClipCategory = "text" | "link" | "image" | "file" | "files";

export type ClipLike = { kind: "text" | "image" | "files"; preview: string; file_count?: number };

const LINK = /^(https?:\/\/|www\.)[^\s]+$/i;

export function clipCategory(item: ClipLike): ClipCategory {
  if (item.kind === "image") return "image";
  if (item.kind === "files") return (item.file_count ?? 0) > 1 ? "files" : "file";
  return LINK.test(item.preview.trim()) ? "link" : "text";
}

/** Header colour for each clipboard card type. */
export const CLIP_TONE: Record<ClipCategory, string> = {
  text: "#6cb6ff",
  link: "#ff7aa8",
  image: "#3fcf8e",
  file: "#f5a524",
  files: "#f5a524",
};

/** Filter chip a card belongs to. */
export function clipFilter(category: ClipCategory): "text" | "link" | "image" | "file" {
  return category === "files" ? "file" : category;
}

/** Host (with a non-default port) without "www." and the rest of the address, for a link card. */
export function linkParts(text: string): { host: string; rest: string } {
  const raw = text.trim();
  try {
    const url = new URL(/^https?:\/\//i.test(raw) ? raw : `https://${raw}`);
    const rest = `${url.pathname === "/" ? "" : url.pathname}${url.search}`;
    let readable = rest;
    try { readable = decodeURIComponent(rest); } catch { /* keep it encoded */ }
    return { host: url.host.replace(/^www\./i, ""), rest: readable };
  } catch {
    return { host: raw, rest: "" };
  }
}

/** Compact age for a card header: now, 4m, 3h, 2d. */
export function shortAge(at: number, now: number): { unit: "now" | "m" | "h" | "d"; count: number } {
  const mins = Math.floor((now - at) / 60000);
  if (mins < 1) return { unit: "now", count: 0 };
  if (mins < 60) return { unit: "m", count: mins };
  const hours = Math.floor(mins / 60);
  if (hours < 24) return { unit: "h", count: hours };
  return { unit: "d", count: Math.floor(hours / 24) };
}
