import { describe, expect, test } from "bun:test";
import { localeUses24h, welcomeState } from "./welcomeGate";

describe("welcomeState", () => {
  test("a fresh profile sees the welcome screen", () => {
    expect(welcomeState({ setting: undefined, firstRunDone: false, tourSeen: false })).toBe("show");
  });

  test("an existing install is adopted silently", () => {
    expect(welcomeState({ setting: undefined, firstRunDone: true, tourSeen: false })).toBe("adopt");
    expect(welcomeState({ setting: null, firstRunDone: false, tourSeen: true })).toBe("adopt");
  });

  test("a welcome left open is shown again, even after the splash marked the first run", () => {
    expect(welcomeState({ setting: "pending", firstRunDone: true, tourSeen: false })).toBe("show");
  });

  test("done stays done", () => {
    expect(welcomeState({ setting: "true", firstRunDone: false, tourSeen: false })).toBe("done");
    expect(welcomeState({ setting: true, firstRunDone: true, tourSeen: true })).toBe("done");
  });
});

describe("localeUses24h", () => {
  test("follows the locale", () => {
    expect(localeUses24h("tr-TR")).toBe(true);
    expect(localeUses24h("de-DE")).toBe(true);
    expect(localeUses24h("en-US")).toBe(false);
  });

  test("a broken locale falls back to 12-hour", () => {
    expect(localeUses24h("not a locale!!")).toBe(false);
  });
});
