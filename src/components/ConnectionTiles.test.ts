import { describe, expect, mock, test } from "bun:test";

let failNext = false;
const calls: Array<[string, unknown]> = [];
mock.module("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args: unknown) => {
    calls.push([cmd, args]);
    if (failNext) throw new Error("denied");
  },
}));
// A module mock lasts for the whole run, so keep the real exports other test files import.
const i18n = { ...(await import("../i18n")) };
mock.module("../i18n", () => ({ ...i18n, useT: () => (k: string) => k }));
mock.module("./ControlCenter.css", () => ({}));

const { toggleWifiRadio, toggleBluetoothRadio } = await import("./ConnectionTiles");

describe("radio toggles", () => {
  test("wifi turns on optimistically and keeps the connected flag off", async () => {
    failNext = false;
    const seen: unknown[] = [];
    await toggleWifiRadio({ enabled: false, connected: true }, (s) => seen.push(s), () => {});
    expect(seen[0]).toEqual({ enabled: true, connected: true });
    expect(calls.at(-1)).toEqual(["set_wifi_state", { enabled: true }]);
  });

  test("wifi turning off clears connected; null status counts as off", async () => {
    failNext = false;
    const seen: unknown[] = [];
    await toggleWifiRadio({ enabled: true, connected: true }, (s) => seen.push(s), () => {});
    expect(seen[0]).toEqual({ enabled: false, connected: false });
    await toggleWifiRadio(null, (s) => seen.push(s), () => {});
    expect(seen[1]).toEqual({ enabled: true, connected: false });
  });

  test("wifi rolls back to the previous status on failure", async () => {
    failNext = true;
    const prev = { enabled: true, connected: true };
    const seen: unknown[] = [];
    await toggleWifiRadio(prev, (s) => seen.push(s), () => {});
    expect(seen).toEqual([{ enabled: false, connected: false }, prev]);
    failNext = false;
  });

  test("bluetooth flips and rolls back on failure", async () => {
    failNext = false;
    const seen: boolean[] = [];
    await toggleBluetoothRadio(false, (v) => seen.push(v));
    expect(seen).toEqual([true]);
    failNext = true;
    const seen2: boolean[] = [];
    await toggleBluetoothRadio(true, (v) => seen2.push(v));
    expect(seen2).toEqual([false, true]);
    failNext = false;
  });
});
