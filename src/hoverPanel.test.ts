import { describe, expect, test } from "bun:test";
import { pickHoverPanel, shouldRemember } from "./hoverPanel";
import type { PanelId } from "./panelOrder";

const all: PanelId[] = ["command-center", "status", "music", "calendar", "shelf", "clipboard", "agents"];
const base = { agentNeedsYou: false, opensPanel: true, downloadsListed: false, available: all, remembered: null, hoverPanel: "command-center" as PanelId };

describe("pickHoverPanel", () => {
  test("an agent that needs you comes first, then listed downloads, then the remembered and hover panels", () => {
    expect(pickHoverPanel({ ...base, agentNeedsYou: true, downloadsListed: true })).toBe("agent");
    expect(pickHoverPanel({ ...base, downloadsListed: true, remembered: "calendar" })).toBe("downloads");
    expect(pickHoverPanel({ ...base, remembered: "calendar" })).toBe("remembered");
    expect(pickHoverPanel(base)).toBe("hover-panel");
  });

  test("downloads take the hover only when hovering opens a panel (not with hover-open off, Legacy Mode or the tour)", () => {
    expect(pickHoverPanel({ ...base, opensPanel: false, downloadsListed: true })).toBe("default");
    expect(pickHoverPanel({ ...base, opensPanel: false, downloadsListed: true, remembered: "calendar" })).toBe("remembered");
  });

  test("panels that are turned off are skipped", () => {
    const noShelf = all.filter((p) => p !== "shelf" && p !== "calendar" && p !== "command-center");
    expect(pickHoverPanel({ ...base, available: noShelf, downloadsListed: true, remembered: "calendar" })).toBe("default");
  });
});

describe("shouldRemember", () => {
  const leave = { overNotice: false, opensPanel: true, shelfByDownloads: false };

  test("remembers the panel the cursor left on", () => {
    expect(shouldRemember("calendar", leave)).toBe(true);
    expect(shouldRemember("shelf", leave)).toBe(true);
    expect(shouldRemember("agent", leave)).toBe(false);
    expect(shouldRemember("calendar", { ...leave, overNotice: true })).toBe(false);
  });

  test("a Shelf opened only because downloads were listed is not remembered", () => {
    expect(shouldRemember("shelf", { ...leave, shelfByDownloads: true })).toBe(false);
    expect(shouldRemember("calendar", { ...leave, shelfByDownloads: true })).toBe(true);
  });

  test("without hover-open only the bigger panels are remembered", () => {
    const off = { ...leave, opensPanel: false };
    expect(shouldRemember("command-center", off)).toBe(true);
    expect(shouldRemember("calendar", off)).toBe(true);
    expect(shouldRemember("status", off)).toBe(false);
    expect(shouldRemember("shelf", off)).toBe(false);
  });
});

describe("hover panel edge cases", () => {
  test("a remembered panel that was turned off falls back to the hover panel, then to the default", () => {
    const noCal = all.filter((p) => p !== "calendar");
    expect(pickHoverPanel({ ...base, available: noCal, remembered: "calendar" })).toBe("hover-panel");
    expect(pickHoverPanel({ ...base, available: noCal, remembered: "calendar", opensPanel: false })).toBe("default");
    expect(pickHoverPanel({ ...base, available: [] })).toBe("default");
  });

  test("an agent that needs you wins even with no panels available or hover-open off", () => {
    expect(pickHoverPanel({ ...base, agentNeedsYou: true, available: [], opensPanel: false })).toBe("agent");
  });

  test("the agent card and notices are never remembered", () => {
    expect(shouldRemember("agent", { overNotice: false, opensPanel: true, shelfByDownloads: false })).toBe(false);
    expect(shouldRemember("shelf", { overNotice: true, opensPanel: true, shelfByDownloads: false })).toBe(false);
  });
});
