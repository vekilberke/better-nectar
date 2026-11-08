// link-sweep: deletes link-blobs objects older than 24 h through the Storage API
// (design section 7.4). Called by pg_cron every 15 minutes with a shared secret from Vault;
// the database part of the sweep (pairings, queue, orphan anonymous users) runs in pg_cron.
import { createClient } from "npm:@supabase/supabase-js@2";

const admin = createClient(Deno.env.get("SUPABASE_URL")!, Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!, {
  auth: { persistSession: false, autoRefreshToken: false },
});

const PAGE = 1000;
const BATCH = 100;
const BUDGET_MS = 20_000;

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

Deno.serve(async (req) => {
  if (req.method !== "POST") return json(405, { error: "method" });
  const secret = req.headers.get("x-link-sweep-secret") ?? "";
  // The Vault secret is 32 random bytes as lowercase hex; anything else never reaches the DB.
  if (!/^[0-9a-f]{64}$/.test(secret)) return json(401, { error: "unauthorized" });
  const { data: valid, error: checkError } = await admin.rpc("link_sweep_check", { s: secret });
  if (checkError || valid !== true) return json(401, { error: "unauthorized" });

  const started = Date.now();
  let removed = 0;
  while (Date.now() - started < BUDGET_MS) {
    const { data: names, error } = await admin.rpc("link_sweep_expired", { lim: PAGE });
    if (error) {
      console.error("link_sweep_expired", error.message);
      return json(500, { error: "failed", removed });
    }
    const list = (names ?? []) as string[];
    if (list.length === 0) break;
    for (let i = 0; i < list.length; i += BATCH) {
      const { error: rmError } = await admin.storage.from("link-blobs").remove(list.slice(i, i + BATCH));
      if (rmError) {
        console.error("storage remove", rmError.message);
        return json(500, { error: "failed", removed });
      }
      removed += Math.min(BATCH, list.length - i);
    }
    if (list.length < PAGE) break;
  }
  return json(200, { removed });
});
