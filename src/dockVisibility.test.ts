import { describe, expect, test } from "bun:test";
import { isDockHidden, nextDockMode, type DockVisibility } from "./dockVisibility";

const base: DockVisibility = {
  mode: "hybrid",
  fullscreen: false,
  shellSurface: false,
  overlapped: false,
  interacting: false,
  startupAnimating: false,
  hybridHidden: false,
};
const hidden = (over: Partial<DockVisibility>) => isDockHidden({ ...base, ...over });

describe("hybrid dock visibility (T-018)", () => {
  test("stays shown over a maximized, overlapping window", () => {
    expect(hidden({ overlapped: true })).toBe(false);
  });

  test("hides with the key and comes back with the next tap", () => {
    expect(hidden({ hybridHidden: true })).toBe(true);
    expect(hidden({ hybridHidden: false })).toBe(false);
  });

  test("hovering the bottom edge does not bring a hidden hybrid dock back", () => {
    expect(hidden({ hybridHidden: true, interacting: true })).toBe(true);
    expect(hidden({ hybridHidden: true, startupAnimating: true })).toBe(true);
  });

  test("still hides for fullscreen apps and shell surfaces", () => {
    expect(hidden({ fullscreen: true })).toBe(true);
    expect(hidden({ shellSurface: true })).toBe(true);
  });

  test("the key state means nothing outside hybrid mode", () => {
    expect(hidden({ mode: "fixed", hybridHidden: true })).toBe(false);
    expect(hidden({ mode: "smart", hybridHidden: true })).toBe(false);
  });
});

describe("other dock modes keep their behaviour", () => {
  test("fixed never hides, even in fullscreen", () => {
    expect(hidden({ mode: "fixed", fullscreen: true, overlapped: true })).toBe(false);
  });

  test("smart hides when overlapped unless hovered or starting", () => {
    expect(hidden({ mode: "smart", overlapped: true })).toBe(true);
    expect(hidden({ mode: "smart", overlapped: true, interacting: true })).toBe(false);
    expect(hidden({ mode: "smart", overlapped: true, startupAnimating: true })).toBe(false);
    expect(hidden({ mode: "smart" })).toBe(false);
  });

  test("peek hides until hovered", () => {
    expect(hidden({ mode: "peek" })).toBe(true);
    expect(hidden({ mode: "peek", interacting: true })).toBe(false);
  });

  test("fullscreen hides smart and peek even while hovered", () => {
    expect(hidden({ mode: "smart", fullscreen: true, interacting: true })).toBe(true);
    expect(hidden({ mode: "peek", shellSurface: true, interacting: true })).toBe(true);
  });

  test("an unknown mode never hides", () => {
    expect(hidden({ mode: "bogus", fullscreen: true })).toBe(false);
  });
});

describe("control center dock tile", () => {
  test("cycles fixed, smart, peek, hybrid", () => {
    expect(nextDockMode("fixed")).toBe("smart");
    expect(nextDockMode("smart")).toBe("peek");
    expect(nextDockMode("peek")).toBe("hybrid");
    expect(nextDockMode("hybrid")).toBe("fixed");
    expect(nextDockMode("bogus")).toBe("fixed");
  });
});
