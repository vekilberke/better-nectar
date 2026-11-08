// Request validation and Storage paths for link-upload (design section 7.1 / 7.4).
// Pure module so it can be tested with bun outside Deno.

export type Dir = "p2c" | "c2p";
export type Kind = "blob" | "mailbox";

export interface UploadRequest {
  topic: string;
  dir: Dir;
  fid: string;
  count: number;
  kind: Kind;
}

export const MAX_CHUNKS = 64;
const TOPIC_RE = /^nl-[0-9a-f]{32}$/;
const FID_RE = /^[0-9a-f]{32}$/;
const JWT_RE = /^[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}$/;

// Cheap shape check before the Auth API round trip, so junk tokens cost nothing.
export function bearerJwt(header: string | null): string | null {
  const token = header?.match(/^Bearer\s+(\S+)$/i)?.[1];
  return token && token.length <= 4096 && JWT_RE.test(token) ? token : null;
}

export interface ReleaseRequest {
  topic: string;
  dir: Dir;
  fid: string;
  kind: Kind;
}

export function parseUploadRequest(body: unknown): UploadRequest | string {
  if (typeof body !== "object" || body === null || Array.isArray(body)) return "bad body";
  const b = body as Record<string, unknown>;
  const allowed = new Set(["topic", "dir", "fid", "count", "kind"]);
  if (Object.keys(b).some((k) => !allowed.has(k))) return "unknown field";
  const { topic, dir, fid, count, kind } = b;
  if (typeof topic !== "string" || !TOPIC_RE.test(topic)) return "bad topic";
  if (dir !== "p2c" && dir !== "c2p") return "bad dir";
  if (typeof fid !== "string" || !FID_RE.test(fid)) return "bad fid";
  if (kind !== "blob" && kind !== "mailbox") return "bad kind";
  if (typeof count !== "number" || !Number.isInteger(count) || count < 1 || count > MAX_CHUNKS) return "bad count";
  if (kind === "mailbox" && count !== 1) return "bad count";
  return { topic, dir, fid, count, kind };
}

// The receiver releases the fid's quota grant after it has downloaded and deleted the
// objects (body: { op: "release", topic, dir, fid, kind }).
export function parseReleaseRequest(body: unknown): ReleaseRequest | string {
  if (typeof body !== "object" || body === null || Array.isArray(body)) return "bad body";
  const b = body as Record<string, unknown>;
  const allowed = new Set(["op", "topic", "dir", "fid", "kind"]);
  if (Object.keys(b).some((k) => !allowed.has(k))) return "unknown field";
  const { op, topic, dir, fid, kind } = b;
  if (op !== "release") return "bad op";
  if (typeof topic !== "string" || !TOPIC_RE.test(topic)) return "bad topic";
  if (dir !== "p2c" && dir !== "c2p") return "bad dir";
  if (typeof fid !== "string" || !FID_RE.test(fid)) return "bad fid";
  if (kind !== "blob" && kind !== "mailbox") return "bad kind";
  return { topic, dir, fid, kind };
}

export function isReleaseBody(body: unknown): boolean {
  return typeof body === "object" && body !== null && !Array.isArray(body) && "op" in body;
}

// All chunk paths of the transfer. URLs are created with upsert, so asking again for the
// same fid after a stall returns fresh URLs and the client re-sends only missing chunks.
export function objectPaths(r: UploadRequest): string[] {
  if (r.kind === "mailbox") return [`${r.topic}/mbox/${r.dir}/${r.fid}`];
  return Array.from({ length: r.count }, (_, i) => `${r.topic}/blob/${r.dir}/${r.fid}/${i}`);
}

// Errors raised by link_upload_authorize and link_upload_release, mapped to HTTP status codes.
const RPC_ERRORS: Record<string, number> = {
  "not member": 403,
  quota: 429,
  "mailbox full": 429,
  "relay full": 507,
  "bad topic": 400,
  "bad dir": 400,
  "bad kind": 400,
  "bad fid": 400,
  "bad count": 400,
};

export function rpcErrorStatus(message: string | undefined): { status: number; code: string } {
  const code = (message ?? "").trim();
  const status = RPC_ERRORS[code];
  return status ? { status, code } : { status: 500, code: "failed" };
}
