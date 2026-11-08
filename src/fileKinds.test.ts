import { describe, expect, test } from "bun:test";
import {
  artShape,
  baseName,
  CLIP_TONE,
  clipCategory,
  clipFilter,
  fileExt,
  fileKind,
  KIND_BADGE,
  KIND_TONE,
  kindLabel,
  linkParts,
  parentFolder,
  shortAge,
  truncateName,
  type FileKind,
} from "./fileKinds";

describe("file kinds", () => {
  test("maps extensions to kinds and shapes", () => {
    const cases: [string, FileKind, string][] = [
      ["Sunset.PNG", "image", "photo"],
      ["Launch.mp4", "video", "film"],
      ["Night.mp3", "audio", "record"],
      ["Invoice-October.pdf", "pdf", "page"],
      ["Brand kit.zip", "archive", "box"],
      ["notes.docx", "doc", "page"],
      ["budget.xlsx", "sheet", "page"],
      ["main.rs", "code", "page"],
      ["setup.exe", "app", "page"],
      ["Projects", "folder", "folder"],
      ["weird.xyz", "generic", "page"],
    ];
    for (const [name, kind, shape] of cases) {
      expect(fileKind(name)).toBe(kind);
      expect(artShape(kind)).toBe(shape);
    }
  });

  test("every kind has a tone", () => {
    const kinds: FileKind[] = ["image", "video", "audio", "pdf", "doc", "sheet", "slides", "archive", "code", "text", "app", "folder", "generic"];
    for (const k of kinds) expect(KIND_TONE[k]).toMatch(/^#[0-9a-f]{6}$/);
    for (const c of ["text", "link", "image", "file", "files"] as const) expect(CLIP_TONE[c]).toMatch(/^#[0-9a-f]{6}$/);
  });

  test("extension edge cases", () => {
    expect(fileExt(".gitignore")).toBe("");
    expect(fileExt("archive.tar.gz")).toBe("gz");
    expect(fileExt("trailing.")).toBe("");
    expect(fileExt("v1.2 final")).toBe("");
    expect(kindLabel("a.pdf")).toBe("PDF");
    expect(kindLabel("a.docx")).toBe("DOCX");
    expect(kindLabel("README")).toBe("FILE");
  });
});

describe("truncateName", () => {
  test("keeps short names", () => {
    expect(truncateName("a.png", 12)).toBe("a.png");
  });
  test("keeps the extension", () => {
    expect(truncateName("Sunset over the bay.png", 12)).toBe("Sunset….png");
    expect(truncateName("Invoice-October.pdf", 12)).toBe("Invoice….pdf");
    const out = truncateName("Launch trailer final.mp4", 12);
    expect(out.endsWith(".mp4")).toBe(true);
    expect(out.length).toBeLessThanOrEqual(13);
  });
  test("falls back to a plain cut when the extension leaves no room", () => {
    expect(truncateName("abcdefghij.markdown", 6)).toBe("abcde…");
    expect(truncateName("no extension at all here", 10)).toBe("no extens…");
  });
});

describe("clipboard helpers", () => {
  test("categories", () => {
    expect(clipCategory({ kind: "text", preview: "hello there" })).toBe("text");
    expect(clipCategory({ kind: "text", preview: " https://github.com/kua8/Nectar " })).toBe("link");
    expect(clipCategory({ kind: "text", preview: "www.example.com/a" })).toBe("link");
    expect(clipCategory({ kind: "text", preview: "see https://x.com" })).toBe("text");
    expect(clipCategory({ kind: "image", preview: "" })).toBe("image");
    expect(clipCategory({ kind: "files", preview: "", file_count: 1 })).toBe("file");
    expect(clipCategory({ kind: "files", preview: "", file_count: 3 })).toBe("files");
    expect(clipFilter("files")).toBe("file");
  });

  test("link parts", () => {
    expect(linkParts("https://www.github.com/kua8/Nectar?tab=readme")).toEqual({ host: "github.com", rest: "/kua8/Nectar?tab=readme" });
    expect(linkParts("https://x.com/a?q=is%3Aopen").rest).toBe("/a?q=is:open");
    expect(linkParts("www.example.com")).toEqual({ host: "example.com", rest: "" });
  });

  test("parent folder", () => {
    expect(parentFolder("C:\\Users\\me\\AppData\\Local\\Temp\\a.png")).toBe("Temp");
    expect(parentFolder("a.png")).toBe("");
  });

  test("short age", () => {
    const now = 10 * 86400000;
    expect(shortAge(now - 20000, now)).toEqual({ unit: "now", count: 0 });
    expect(shortAge(now - 2 * 60000, now)).toEqual({ unit: "m", count: 2 });
    expect(shortAge(now - 3 * 3600000, now)).toEqual({ unit: "h", count: 3 });
    expect(shortAge(now - 2 * 86400000, now)).toEqual({ unit: "d", count: 2 });
  });
});

describe("edge cases (T-023 round 1)", () => {
  test("truncateName never grows past max + 1 and keeps a short extension", () => {
    const names = ["Ünïcödé ñame — 日本語のファイル名.pdf", "x".repeat(200) + ".png", "a.b", "", ".gitignore", "Averyveryveryverylongnamewithoutanyspacesatall.png", "archive.tar.gz", "name with trailing space .txt"];
    for (const name of names) {
      for (const max of [4, 8, 12, 14, 30]) {
        const out = truncateName(name, max);
        expect(out.length).toBeLessThanOrEqual(Math.max(max, name.length > max ? max + 1 : name.length));
        if (name.length > max && fileExt(name) && name.length - name.lastIndexOf(".") <= 7 && max >= 10) expect(out.endsWith(`.${fileExt(name)}`)).toBe(true);
      }
    }
    expect(truncateName("", 10)).toBe("");
  });

  test("truncateName cuts a long extension as plain text", () => {
    const out = truncateName("report.verylongextension", 10);
    expect(out.length).toBeLessThanOrEqual(10);
    expect(out.endsWith("…")).toBe(true);
  });

  test("link parts: odd input", () => {
    expect(linkParts("HTTP://Example.COM/Path")).toEqual({ host: "example.com", rest: "/Path" });
    expect(linkParts("https://x.com/100%").rest).toBe("/100%");
    expect(linkParts("https://x.com/").rest).toBe("");
    expect(linkParts("https://").host).toBe("https://");
  });

  test("categories: odd input", () => {
    expect(clipCategory({ kind: "text", preview: "https://a.com\nsecond line" })).toBe("text");
    expect(clipCategory({ kind: "text", preview: "" })).toBe("text");
    expect(clipCategory({ kind: "files", preview: "" })).toBe("file");
    expect(clipCategory({ kind: "files", preview: "", file_count: 0 })).toBe("file");
    expect(clipCategory({ kind: "text", preview: "HTTPS://A.COM" })).toBe("link");
  });

  test("short age: boundaries and clock skew", () => {
    const now = 100 * 86400000;
    expect(shortAge(now - 59999, now)).toEqual({ unit: "now", count: 0 });
    expect(shortAge(now - 60000, now)).toEqual({ unit: "m", count: 1 });
    expect(shortAge(now - 59 * 60000 - 59999, now)).toEqual({ unit: "m", count: 59 });
    expect(shortAge(now - 3600000, now)).toEqual({ unit: "h", count: 1 });
    expect(shortAge(now - 86400000, now)).toEqual({ unit: "d", count: 1 });
    expect(shortAge(now + 5 * 60000, now)).toEqual({ unit: "now", count: 0 });
  });

  test("every clip category has a tone and a filter", () => {
    for (const c of ["text", "link", "image", "file", "files"] as const) {
      expect(CLIP_TONE[c]).toMatch(/^#[0-9a-f]{6}$/i);
      expect(["text", "link", "image", "file"]).toContain(clipFilter(c));
    }
  });

  // I-282: dotfiles and extension-less files have no "extension"; the filesystem's answer decides.
  test("a known file without an extension is not a folder", () => {
    expect(fileKind(".gitignore", false)).toBe("generic");
    expect(fileKind("LICENSE", false)).toBe("generic");
    expect(kindLabel(".gitignore")).toBe("FILE");
  });

  test("a known folder is a folder whatever its name", () => {
    expect(fileKind("Projects", true)).toBe("folder");
    expect(fileKind("site.v2", true)).toBe("folder");
    expect(fileKind("archive.zip", true)).toBe("folder");
  });

  test("without the filesystem's answer the name decides", () => {
    expect(fileKind("Projects")).toBe("folder");
    expect(fileKind("Projects", null)).toBe("folder");
    expect(fileKind("notes.txt", false)).toBe("text");
  });
});

describe("edge cases (T-023 round 2)", () => {
  const luminance = (hex: string) => {
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const whiteContrast = (hex: string) => 1.05 / (luminance(hex) + 0.05);

  // I-298: white 10.5px badge text was 2.5:1 on EXE and TXT, 2.8 on PPTX, 3.0-3.6 on PDF, XLSX, code (needs 4.5:1).
  test("white page badges reach 4.5:1 on every badge fill", () => {
    for (const kind of Object.keys(KIND_TONE) as FileKind[]) {
      expect(whiteContrast(KIND_BADGE[kind])).toBeGreaterThanOrEqual(4.5);
    }
  });

  test("extensions: case, several dots, trailing dot, odd characters", () => {
    expect(fileExt("FILE.TAR.GZ")).toBe("gz");
    expect(fileExt("report.")).toBe("");
    expect(fileExt("a.b c")).toBe("");
    expect(fileExt("x.中文")).toBe("中文");
    expect(fileExt("a.verylongext")).toBe("");
    expect(kindLabel("book.markdown")).toBe("MARK");
    expect(kindLabel("Q3 Budget.XLSX")).toBe("XLSX");
  });

  test("paths: forward slashes, drive roots, UNC shares, trailing separator", () => {
    expect(baseName("C:/a/b/c.txt")).toBe("c.txt");
    expect(baseName(["C:", "a", "b", ""].join("\\"))).toBe("b");
    expect(baseName("")).toBe("");
    expect(parentFolder("C:/a/b.txt")).toBe("a");
    expect(parentFolder("C:")).toBe("");
    expect(parentFolder(["", "", "server", "share", "a.png"].join("\\"))).toBe("share");
  });

  test("filters: every category lands on exactly one chip, files and file share a chip", () => {
    expect(clipFilter("files")).toBe(clipFilter("file"));
    expect(new Set((["text", "link", "image", "file", "files"] as const).map(clipFilter)).size).toBe(4);
  });

  test("a card with files but no recorded count is a single file", () => {
    expect(clipCategory({ kind: "files", preview: "", file_count: 1 })).toBe("file");
    expect(clipCategory({ kind: "files", preview: "", file_count: 2 })).toBe("files");
    expect(clipCategory({ kind: "files", preview: "", file_count: 1000 })).toBe("files");
  });

  // I-296: slicing by UTF-16 unit cut an emoji in half and left a lone surrogate, drawn as a box.
  test("truncateName does not split an emoji", () => {
    for (const name of ["😀".repeat(12) + ".png", "日本語😀abcdefghijk.pdf", "a😀😀😀😀😀😀😀😀😀😀"]) {
      for (let max = 5; max < 14; max++) expect(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(truncateName(name, max))).toBe(false);
    }
    expect(truncateName("😀".repeat(20) + ".png", 10)).toBe("😀".repeat(5) + "….png");
  });

  // I-297: the link card showed the host only, so a dev server link lost its port.
  test("link parts keep a non-default port", () => {
    expect(linkParts("http://localhost:3000/a").host).toBe("localhost:3000");
    expect(linkParts("https://www.example.com:443/a").host).toBe("example.com");
  });
});
