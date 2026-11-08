import { describe, expect, test } from "bun:test";
import {
  bearerJwt, isReleaseBody, MAX_CHUNKS, objectPaths, parseReleaseRequest, parseUploadRequest, rpcErrorStatus, type UploadRequest,
} from "./paths";

const topic = "nl-" + "ab".repeat(16);
const fid = "0f".repeat(16);

describe("parseUploadRequest", () => {
  test("accepts a blob request", () => {
    expect(parseUploadRequest({ topic, dir: "p2c", fid, count: 3, kind: "blob" })).toEqual({
      topic, dir: "p2c", fid, count: 3, kind: "blob",
    });
  });

  test("rejects malformed fields", () => {
    const base = { topic, dir: "c2p", fid, count: 1, kind: "mailbox" };
    expect(parseUploadRequest(null)).toBe("bad body");
    expect(parseUploadRequest([])).toBe("bad body");
    expect(parseUploadRequest({ ...base, extra: 1 })).toBe("unknown field");
    expect(parseUploadRequest({ ...base, topic: "nlp-" + "ab".repeat(16) })).toBe("bad topic");
    expect(parseUploadRequest({ ...base, topic: topic + "/../x" })).toBe("bad topic");
    expect(parseUploadRequest({ ...base, dir: "up" })).toBe("bad dir");
    expect(parseUploadRequest({ ...base, fid: "../" + fid.slice(3) })).toBe("bad fid");
    expect(parseUploadRequest({ ...base, fid: fid.toUpperCase() })).toBe("bad fid");
    expect(parseUploadRequest({ ...base, kind: "other" })).toBe("bad kind");
    expect(parseUploadRequest({ ...base, count: 2 })).toBe("bad count");
    expect(parseUploadRequest({ ...base, kind: "blob", count: 0 })).toBe("bad count");
    expect(parseUploadRequest({ ...base, kind: "blob", count: 1.5 })).toBe("bad count");
    expect(parseUploadRequest({ ...base, kind: "blob", count: MAX_CHUNKS + 1 })).toBe("bad count");
    expect(parseUploadRequest({ ...base, kind: "blob", count: "2" })).toBe("bad count");
  });
});

describe("parseReleaseRequest", () => {
  test("accepts a release and routes it by op", () => {
    const body = { op: "release", topic, dir: "p2c", fid, kind: "blob" };
    expect(isReleaseBody(body)).toBe(true);
    expect(isReleaseBody({ topic, dir: "p2c", fid, count: 1, kind: "blob" })).toBe(false);
    expect(isReleaseBody(null)).toBe(false);
    expect(parseReleaseRequest(body)).toEqual({ topic, dir: "p2c", fid, kind: "blob" });
  });

  test("rejects malformed releases", () => {
    const base = { op: "release", topic, dir: "c2p", fid, kind: "mailbox" };
    expect(parseReleaseRequest([])).toBe("bad body");
    expect(parseReleaseRequest({ ...base, op: "upload" })).toBe("bad op");
    expect(parseReleaseRequest({ ...base, count: 1 })).toBe("unknown field");
    expect(parseReleaseRequest({ ...base, topic: topic + "/x" })).toBe("bad topic");
    expect(parseReleaseRequest({ ...base, dir: "up" })).toBe("bad dir");
    expect(parseReleaseRequest({ ...base, fid: "x" })).toBe("bad fid");
    expect(parseReleaseRequest({ ...base, kind: "other" })).toBe("bad kind");
  });
});

describe("objectPaths", () => {
  test("one object per chunk under the topic and direction", () => {
    const r: UploadRequest = { topic, dir: "p2c", fid, count: 3, kind: "blob" };
    expect(objectPaths(r)).toEqual([0, 1, 2].map((i) => `${topic}/blob/p2c/${fid}/${i}`));
  });

  test("mailbox item path", () => {
    const r: UploadRequest = { topic, dir: "c2p", fid, count: 1, kind: "mailbox" };
    expect(objectPaths(r)).toEqual([`${topic}/mbox/c2p/${fid}`]);
  });
});

describe("bearerJwt", () => {
  test("accepts only a JWT-shaped bearer token", () => {
    const jwt = "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJ4In0abc.c2lnbmF0dXJlX2hlcmU";
    expect(bearerJwt(`Bearer ${jwt}`)).toBe(jwt);
    expect(bearerJwt(`bearer ${jwt}`)).toBe(jwt);
    expect(bearerJwt(null)).toBeNull();
    expect(bearerJwt(jwt)).toBeNull();
    expect(bearerJwt("Bearer " + "x".repeat(40))).toBeNull();
    expect(bearerJwt(`Bearer ${jwt} extra`)).toBeNull();
    expect(bearerJwt("Bearer a.b.c")).toBeNull();
    expect(bearerJwt(`Bearer ${jwt}${"a".repeat(4096)}`)).toBeNull();
  });
});

describe("rpcErrorStatus", () => {
  test("maps known errors and hides others", () => {
    expect(rpcErrorStatus("not member")).toEqual({ status: 403, code: "not member" });
    expect(rpcErrorStatus("quota")).toEqual({ status: 429, code: "quota" });
    expect(rpcErrorStatus("relay full")).toEqual({ status: 507, code: "relay full" });
    expect(rpcErrorStatus("bad fid")).toEqual({ status: 400, code: "bad fid" });
    expect(rpcErrorStatus("permission denied for table x")).toEqual({ status: 500, code: "failed" });
    expect(rpcErrorStatus(undefined)).toEqual({ status: 500, code: "failed" });
  });
});
