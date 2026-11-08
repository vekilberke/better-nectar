import type { CSSProperties } from "react";
import { AppWindow, File, FileArchive, FileCode, FileSpreadsheet, FileText, Film, Folder, Image, Music, Play, Presentation } from "lucide-react";
import { artShape, fileKind, KIND_BADGE, KIND_TONE, kindLabel, type FileKind } from "../fileKinds";

type ArtProps = {
  name: string;
  /** Shell thumbnail (data URL) when Windows has one. */
  thumb?: string | null;
  /** First lines of a text file. */
  preview?: string | null;
  /** Draws a sticky note for a text snippet instead of a file. */
  note?: string | null;
  /** Whether the path is a folder, when the filesystem was asked. */
  dir?: boolean | null;
};

const tone = (kind: FileKind) => ({ "--fa-tone": KIND_TONE[kind], "--fa-badge": KIND_BADGE[kind] }) as CSSProperties;

/** Large drawn artwork for a shelf tile: photo card, film strip, record, page, box or folder. */
export function FileArt({ name, thumb, preview, note, dir }: ArtProps) {
  if (note != null) {
    return (
      <div className="fa fa-note-wrap" aria-hidden="true">
        <div className={`fa-note${note.length < 24 ? " short" : ""}`}>{note}</div>
      </div>
    );
  }
  const kind = fileKind(name, dir);
  const shape = artShape(kind);
  const img = thumb ? <img src={thumb} alt="" draggable={false} /> : null;

  return (
    <div className={`fa fa-${shape}-wrap`} style={tone(kind)} aria-hidden="true">
      {shape === "photo" && (
        <div className="fa-photo">
          <div className="fa-photo-img">{img ?? <span className="fa-scene" />}</div>
        </div>
      )}
      {shape === "film" && (
        <div className="fa-film">
          <div className="fa-film-frame">
            {img}
            <span className="fa-play"><Play size={11} strokeWidth={0} fill="currentColor" /></span>
          </div>
        </div>
      )}
      {shape === "record" && (
        <div className="fa-record">
          <span className="fa-record-label">{img}</span>
        </div>
      )}
      {shape === "box" && (
        <div className="fa-box">
          <span className="fa-box-tape" />
          <span className="fa-badge fa-box-badge">{kindLabel(name)}</span>
        </div>
      )}
      {shape === "folder" && (
        <div className="fa-folder">
          <span className="fa-folder-back" />
          <span className="fa-folder-front" />
        </div>
      )}
      {shape === "page" && (
        <div className="fa-page">
          <div className="fa-page-body">
            {img ?? (preview ? <pre className="fa-page-text">{preview}</pre> : (
              <>
                <span className="fa-line" />
                <span className="fa-line" />
                <span className="fa-line short" />
              </>
            ))}
          </div>
          <span className="fa-page-fold" />
          <span className="fa-badge">{kindLabel(name)}</span>
        </div>
      )}
    </div>
  );
}

const ICONS: Record<FileKind, typeof File> = {
  image: Image,
  video: Film,
  audio: Music,
  pdf: FileText,
  doc: FileText,
  sheet: FileSpreadsheet,
  slides: Presentation,
  archive: FileArchive,
  code: FileCode,
  text: FileText,
  app: AppWindow,
  folder: Folder,
  generic: File,
};

/** Small coloured tile with the kind's icon, for clipboard file cards. */
export function FileGlyph({ name }: { name: string }) {
  const kind = fileKind(name);
  const Icon = ICONS[kind];
  return (
    <span className="fa-glyph" style={tone(kind)} aria-hidden="true">
      <Icon size={17} strokeWidth={2.1} />
    </span>
  );
}
