import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { findLeaks, selfTest } from "./check-shared-build.mjs";

test("leak list hits the relay and the PWA, not the allowed words", () => {
  expect(selfTest()).toEqual([]);
  expect(findLeaks(Buffer.from("nothing here"))).toEqual([]);
});

test("package.json, tauri.conf.json and Cargo.toml agree on the version", () => {
  const root = join(import.meta.dir, "..");
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
  const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8")).version;
  const cargo = /^version = "([^"]*)"/m.exec(readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf8"))?.[1];
  expect(conf).toBe(pkg);
  expect(cargo).toBe(pkg);
});
