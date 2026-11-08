import { describe, expect, test } from "bun:test";
import {
  EMPTY_PRIVACY, formatApps, normalizePrivacy, privacyChipWidth, privacyItems, privacyLaneWidth, privacyLines, samePrivacy, tabStripReserve,
  PRIVACY_DOT, PRIVACY_GAP, PRIVACY_ICON, PRIVACY_LEAD,
} from "./privacyIndicator";

const labels = { mic: "Microphone", camera: "Camera", muted: "Microphone muted", partial: "Some microphones are still on" };

describe("privacy state (T-019)", () => {
  test("malformed payloads read as nothing in use", () => {
    expect(normalizePrivacy(null)).toEqual(EMPTY_PRIVACY);
    expect(normalizePrivacy("x")).toEqual(EMPTY_PRIVACY);
    expect(normalizePrivacy({ mic: "Zoom", camera: [1, null] })).toEqual(EMPTY_PRIVACY);
  });

  test("names are trimmed and unique, order kept", () => {
    const s = normalizePrivacy({ mic: [" Zoom ", "Teams", "zoom", ""], camera: ["Zoom"], muted: false, meeting: true });
    expect(s.mic).toEqual(["Zoom", "Teams"]);
    expect(s.camera).toEqual(["Zoom"]);
  });

  test("muted stays after meeting mode is turned off while Nectar's mute holds (I-208)", () => {
    expect(normalizePrivacy({ muted: true, meeting: false }).muted).toBe(true);
    expect(normalizePrivacy({ muted: true, meeting: true }).muted).toBe(true);
  });

  test("partial only counts while muted (I-207)", () => {
    expect(normalizePrivacy({ muted: true, partial: true, meeting: true }).partial).toBe(true);
    expect(normalizePrivacy({ muted: false, partial: true, meeting: true }).partial).toBe(false);
    expect(normalizePrivacy({ muted: true, partial: "true", meeting: true }).partial).toBe(false);
    const full = normalizePrivacy({ muted: true, meeting: true });
    expect(samePrivacy(full, normalizePrivacy({ muted: true, partial: true, meeting: true }))).toBe(false);
  });

  test("a partial mute still takes the muted mark and says a mic is on", () => {
    const s = normalizePrivacy({ mic: ["Zoom"], muted: true, partial: true, meeting: true });
    expect(privacyItems(s)).toEqual(["muted", "mic"]);
    expect(privacyLines(s, labels)).toEqual(["Some microphones are still on", "Microphone: Zoom"]);
  });

  test("samePrivacy compares lists and flags", () => {
    const a = normalizePrivacy({ mic: ["Zoom"], meeting: true });
    expect(samePrivacy(a, normalizePrivacy({ mic: ["Zoom"], meeting: true }))).toBe(true);
    expect(samePrivacy(a, normalizePrivacy({ mic: ["Zoom", "Teams"], meeting: true }))).toBe(false);
    expect(samePrivacy(a, normalizePrivacy({ mic: ["Zoom"], meeting: true, muted: true }))).toBe(false);
  });

  test("items go muted, mic, camera", () => {
    expect(privacyItems(EMPTY_PRIVACY)).toEqual([]);
    expect(privacyItems(normalizePrivacy({ camera: ["Zoom"] }))).toEqual(["camera"]);
    expect(privacyItems(normalizePrivacy({ mic: ["Zoom"], camera: ["Zoom"], muted: true, meeting: true }))).toEqual(["muted", "mic", "camera"]);
  });
});

describe("privacy widths (T-019)", () => {
  test("nothing in use takes no room", () => {
    expect(privacyLaneWidth([])).toBe(0);
    expect(privacyChipWidth([])).toBe(0);
    expect(tabStripReserve([])).toBe(0);
  });

  test("lane is the lead gap plus dots, icon and gaps", () => {
    expect(privacyLaneWidth(["mic"])).toBe(PRIVACY_LEAD + PRIVACY_DOT);
    expect(privacyLaneWidth(["mic", "camera"])).toBe(PRIVACY_LEAD + 2 * PRIVACY_DOT + PRIVACY_GAP);
    expect(privacyLaneWidth(["muted", "mic", "camera"])).toBe(PRIVACY_LEAD + PRIVACY_ICON + 2 * PRIVACY_DOT + 2 * PRIVACY_GAP);
  });

  test("the lane stays small next to the clock, cat and agent bar", () => {
    expect(privacyLaneWidth(["muted", "mic", "camera"])).toBeLessThanOrEqual(40);
  });

  test("the open notch keeps the chip's room on both sides of the centred tabs", () => {
    const items = ["mic", "camera"] as const;
    const tabs = 24 + 6 * 24 + 13; // tabStripMinWidth(5) in NotchTabStrip.tsx
    const width = tabs + tabStripReserve(items);
    // Centred tabs end at (width + tabs) / 2 minus the notch's 12px padding; the chip starts at width - 12 - chip.
    const tabsRight = (width + tabs) / 2 - 12;
    const chipLeft = width - 12 - privacyChipWidth(items);
    expect(chipLeft).toBeGreaterThanOrEqual(tabsRight);
  });
});

describe("privacy text (T-019)", () => {
  test("long app lists are shortened", () => {
    expect(formatApps(["Zoom"])).toBe("Zoom");
    expect(formatApps(["Zoom", "Teams"])).toBe("Zoom, Teams");
    expect(formatApps(["Zoom", "Teams", "Discord", "Chrome"])).toBe("Zoom, Teams +2");
  });

  test("one line per thing in use, muted first", () => {
    const s = normalizePrivacy({ mic: ["Zoom", "Teams", "Discord"], camera: ["Zoom"], muted: true, meeting: true });
    expect(privacyLines(s, labels)).toEqual(["Microphone muted", "Microphone: Zoom, Teams +1", "Camera: Zoom"]);
    expect(privacyLines(EMPTY_PRIVACY, labels)).toEqual([]);
  });
});

describe("privacy edge cases (T-019, tester)", () => {
  test("extra payload fields and non-boolean flags are ignored", () => {
    const s = normalizePrivacy({ mic: ["A"], meeting: "true", muted: "true", extra: 1 });
    expect(s.meeting).toBe(false);
    expect(s.muted).toBe(false);
  });

  test("names differing only by case collapse, first spelling wins", () => {
    expect(normalizePrivacy({ mic: ["Zoom", "ZOOM"] }).mic).toEqual(["Zoom"]);
  });

  test("samePrivacy distinguishes mic from camera lists", () => {
    expect(samePrivacy(normalizePrivacy({ mic: ["Zoom"] }), normalizePrivacy({ camera: ["Zoom"] }))).toBe(false);
  });

  test("widths grow with each mark and the chip adds its padding", () => {
    expect(privacyLaneWidth(["muted"])).toBeLessThan(privacyLaneWidth(["muted", "mic"]));
    expect(privacyChipWidth(["mic"])).toBe(PRIVACY_DOT + 10);
  });

  test("formatApps with exactly max entries has no suffix", () => {
    expect(formatApps(["A", "B"], 2)).toBe("A, B");
    expect(formatApps([])).toBe("");
  });
});
