// link-upload: the only way to write to the link-blobs bucket (design section 7.1).
// Checks the caller's membership and direction plus quotas in link_upload_authorize, then
// returns one signed upload URL per object. With op "release" the receiver frees the fid's
// quota grant once it has consumed the transfer (link_upload_release). Deployed with
// verify_jwt off because it validates the user token itself with auth.getUser.
import { createClient } from "npm:@supabase/supabase-js@2";
import { bearerJwt, isReleaseBody, objectPaths, parseReleaseRequest, parseUploadRequest, rpcErrorStatus } from "./paths.ts";

const admin = createClient(Deno.env.get("SUPABASE_URL")!, Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!, {
  auth: { persistSession: false, autoRefreshToken: false },
});

const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Headers": "authorization, apikey, content-type, x-client-info",
  "Access-Control-Allow-Methods": "POST, OPTIONS",
};

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { ...CORS, "Content-Type": "application/json" } });
}

Deno.serve(async (req) => {
  if (req.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
  if (req.method !== "POST") return json(405, { error: "method" });

  const token = bearerJwt(req.headers.get("authorization"));
  if (!token) return json(401, { error: "unauthorized" });
  const { data: userData, error: userError } = await admin.auth.getUser(token);
  if (userError || !userData.user) return json(401, { error: "unauthorized" });

  const body = await req.json().catch(() => null);
  if (isReleaseBody(body)) {
    const rel = parseReleaseRequest(body);
    if (typeof rel === "string") return json(400, { error: rel });
    const { error: relError } = await admin.rpc("link_upload_release", {
      p_uid: userData.user.id,
      p_topic: rel.topic,
      p_dir: rel.dir,
      p_kind: rel.kind,
      p_fid: rel.fid,
    });
    if (relError) {
      const { status, code } = rpcErrorStatus(relError.message);
      if (status === 500) console.error("link_upload_release", relError.message);
      return json(status, { error: code });
    }
    return json(200, { released: true });
  }

  const parsed = parseUploadRequest(body);
  if (typeof parsed === "string") return json(400, { error: parsed });

  const { error: authzError } = await admin.rpc("link_upload_authorize", {
    p_uid: userData.user.id,
    p_topic: parsed.topic,
    p_dir: parsed.dir,
    p_kind: parsed.kind,
    p_fid: parsed.fid,
    p_count: parsed.count,
  });
  if (authzError) {
    const { status, code } = rpcErrorStatus(authzError.message);
    if (status === 500) console.error("link_upload_authorize", authzError.message);
    return json(status, { error: code });
  }

  const bucket = admin.storage.from("link-blobs");
  const uploads = [];
  for (const path of objectPaths(parsed)) {
    // upsert: only the direction's single member gets URLs, so overwriting is safe and lets a
    // stalled transfer resume under the same fid.
    const { data, error } = await bucket.createSignedUploadUrl(path, { upsert: true });
    if (error || !data) {
      console.error("createSignedUploadUrl", error?.message);
      return json(500, { error: "failed" });
    }
    uploads.push({ path: data.path, token: data.token, url: data.signedUrl });
  }
  return json(200, { uploads });
});
