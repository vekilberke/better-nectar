import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// Bun has no ChaCha20-Poly1305 in node:crypto, so the independent check runs under Node.
const node = spawnSync("node", ["--version"], { encoding: "utf8" });

test.skipIf(node.status !== 0)("phone/test-vectors.json matches an independent Node implementation", () => {
  const script = fileURLToPath(new URL("./check-phone-vectors.mjs", import.meta.url));
  const run = spawnSync("node", [script], { encoding: "utf8" });
  expect(run.stderr).toBe("");
  expect(run.stdout).toContain("vectors match");
  expect(run.status).toBe(0);
});
