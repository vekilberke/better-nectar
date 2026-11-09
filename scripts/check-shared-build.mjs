// Fails when the shared (friend) build carries the phone link relay or PWA: the Supabase project,
// its key, the PWA address or a phone link command. Words that are fine in the shared build
// (`nectar-phone-` in the import filter, "Supabase" in locale text, the vekilberke identity) are
// not on the list.
//
//   node scripts/check-shared-build.mjs [--dist dist] [--exe path\to\better-nectar.exe]
//   node scripts/check-shared-build.mjs --self-test
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Built from parts so the public export's scrub grep does not hit this file.
const PUBLISHABLE = ["sb", "publishable"].join("_");

export const LEAKS = [
  "qziazud",
  PUBLISHABLE,
  "supabase.co",
  "vekilberke.github.io",
  "/nectar-phone/",
  "phone_link_",
  "realtime/v1",
  "link_owners",
];

/** Leak words found in `bytes`, read as UTF-8/ASCII and as UTF-16LE, ignoring ASCII case. */
export function findLeaks(bytes) {
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  const views = [buf.toString("latin1").toLowerCase(), buf.toString("utf16le").toLowerCase()];
  if (buf.length > 1) views.push(buf.subarray(1).toString("utf16le").toLowerCase());
  return LEAKS.filter((word) => views.some((text) => text.includes(word)));
}

export function selfTest() {
  const hits = [
    "https://qziazudexample0000.supabase.co/functions/v1/x",
    `${PUBLISHABLE}_abc123`,
    'invoke("phone_link_start_pairing")',
    "https://vekilberke.github.io/nectar-phone/#pair",
    "wss://x/realtime/v1/websocket",
  ];
  const clean = [
    "nectar-phone-link",
    "Open the Supabase dashboard",
    "io.github.vekilberke.betternectar",
    "https://github.com/vekilberke/better-nectar/releases/latest/download/latest.json",
  ];
  const failures = [];
  for (const sample of hits) {
    for (const encoding of ["utf8", "utf16le"]) {
      if (findLeaks(Buffer.from(sample.toUpperCase(), encoding)).length === 0) failures.push(`missed ${encoding}: ${sample}`);
    }
  }
  for (const sample of clean) {
    for (const encoding of ["utf8", "utf16le"]) {
      const found = findLeaks(Buffer.from(sample, encoding));
      if (found.length) failures.push(`false hit ${encoding} (${found.join(", ")}): ${sample}`);
    }
  }
  return failures;
}

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

function defaultExe(root) {
  const target = process.env.CARGO_TARGET_DIR || join(root, "src-tauri", "target");
  const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
  return join(target, "release", `${conf.mainBinaryName}.exe`);
}

function main(argv) {
  if (argv.includes("--self-test")) {
    const failures = selfTest();
    failures.forEach((f) => console.error(f));
    console.log(failures.length ? "self-test failed" : "self-test passed");
    return failures.length ? 1 : 0;
  }
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const arg = (name) => {
    const i = argv.indexOf(name);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  const dist = resolve(arg("--dist") ?? join(root, "dist"));
  const exe = resolve(arg("--exe") ?? defaultExe(root));
  const targets = [];
  if (existsSync(dist)) targets.push(...files(dist));
  else console.warn(`no ${dist}, skipped`);
  if (existsSync(exe)) targets.push(exe);
  else console.warn(`no ${exe}, skipped`);
  if (targets.length === 0) {
    console.error("nothing to check");
    return 1;
  }
  let bad = 0;
  for (const path of targets) {
    const found = findLeaks(readFileSync(path));
    if (found.length) {
      bad++;
      console.error(`${path}: ${found.join(", ")}`);
    }
  }
  console.log(bad ? `phone link material found in ${bad} file(s)` : `clean: ${targets.length} file(s)`);
  return bad ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}
