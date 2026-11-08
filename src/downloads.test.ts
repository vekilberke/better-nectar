import { describe, expect, test } from "bun:test";
import { downloadsListHeight, downloadsSummary, formatBytes, formatSpeed, visibleDownloads, DOWNLOAD_ROW_H, type Download } from "./downloads";

const d = (id: number, done = false, speed = 0): Download => ({ id, name: `f${id}.zip`, bytes: 1000, speed, done });

describe("formatBytes", () => {
  test("picks a 1024-based unit with at most one decimal", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(812)).toBe("812 B");
    expect(formatBytes(1024)).toBe("1 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(4.2 * 1024 * 1024)).toBe("4.2 MB");
    expect(formatBytes(250 * 1024 * 1024)).toBe("250 MB");
    expect(formatBytes(1.25 * 1024 ** 3)).toBe("1.3 GB");
    expect(formatBytes(-5)).toBe("0 B");
  });

  test("a value that rounds up to 1024 moves to the next unit", () => {
    expect(formatBytes(1048500)).toBe("1 MB");
    expect(formatBytes(1024 ** 3 - 1)).toBe("1 GB");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1023.4 * 1024)).toBe("1023 KB");
    expect(formatBytes(2000 * 1024 ** 4)).toBe("2000 TB");
  });

  test("speed adds /s", () => {
    expect(formatSpeed(2 * 1024 * 1024)).toBe("2 MB/s");
  });
});

describe("downloadsSummary", () => {
  test("counts running and finished downloads and sums the speed of running ones", () => {
    expect(downloadsSummary([])).toEqual({ active: 0, done: 0, speed: 0 });
    expect(downloadsSummary([d(1, false, 100), d(2, true, 0), d(3, false, 50)])).toEqual({ active: 2, done: 1, speed: 150 });
  });
});

describe("downloads list in the Shelf panel", () => {
  test("running downloads come first and at most three rows show", () => {
    const { rows, more } = visibleDownloads([d(1, true), d(2), d(3, true), d(4), d(5)]);
    expect(rows.map((r) => r.id)).toEqual([2, 4, 5]);
    expect(more).toBe(2);
  });

  test("height grows with the rows and has room for the +N line", () => {
    expect(downloadsListHeight([])).toBe(0);
    expect(downloadsListHeight([d(1)])).toBe(DOWNLOAD_ROW_H + 6);
    expect(downloadsListHeight([d(1), d(2), d(3), d(4)])).toBe(3 * DOWNLOAD_ROW_H + 16 + 6);
  });
});
