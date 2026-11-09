// The owner's own build: phone link on, the upstream identity (`com.kua8.nectar`, nectar.exe) and
// no updater. `bun run build:own` (release exe, no installer) or `bun run dev:own`.
import { spawnSync } from "node:child_process";

const dev = process.argv[2] === "dev";
const own = ["--features", "phone-link", "--config", "src-tauri/tauri.own.conf.json"];
const args = dev ? ["run", "tauri", "dev", ...own] : ["run", "tauri", "build", "--no-bundle", ...own];
const run = spawnSync("bun", [...args, ...process.argv.slice(3)], {
  stdio: "inherit",
  env: { ...process.env, VITE_PHONE_LINK: "1" },
  shell: process.platform === "win32",
});
process.exit(run.status ?? 1);
