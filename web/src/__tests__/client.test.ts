import { describe, expect, it } from "vitest";
import { ApiError, enc, getAll, request, setCsrfToken } from "../api/client";
import * as api from "../api/endpoints";
import { apiError, fakeApi, json } from "../test/fakeApi";

describe("API client", () => {
  it("sends the CSRF header on non-GET requests only, with same-origin credentials", async () => {
    const fake = fakeApi({
      "GET /api/v1/me": json(200, { name: "owner" }),
      "POST /api/v1/repos/acme/demo/tickets/1/comments": json(201, { id: "c1" }),
      "DELETE /api/v1/members/aiko/tokens/tok_1": { status: 204 },
    });
    setCsrfToken("csrf-xyz");
    await api.me();
    await api.tickets.addComment("acme/demo", "1", "hello");
    await api.members.revokeToken("aiko", "tok_1");

    const [getReq, postReq, deleteReq] = fake.requests;
    expect(getReq?.headers["X-Branvil-CSRF"]).toBeUndefined();
    expect(postReq?.headers["X-Branvil-CSRF"]).toBe("csrf-xyz");
    expect(postReq?.headers["Content-Type"]).toBe("application/json");
    expect(postReq?.headers["Idempotency-Key"]).toMatch(/^[0-9a-f-]{36}$/);
    expect(postReq?.body).toEqual({ body: "hello" });
    expect(deleteReq?.headers["X-Branvil-CSRF"]).toBe("csrf-xyz");
    for (const r of fake.requests) {
      expect(r.credentials).toBe("same-origin");
      expect(r.headers["Authorization"]).toBeUndefined();
    }
  });

  it("encodes path parameters as one segment", async () => {
    const fake = fakeApi({ "GET /api/v1/repos/acme/demo": json(200, {}) });
    await api.repos.get("acme/demo");
    expect(enc("acme/demo")).toBe("acme%2Fdemo");
    // The raw URL keeps the slashes encoded.
    const fetchMock = globalThis.fetch as unknown as { mock: { calls: [string][] } };
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/repos/acme%2Fdemo");
    expect(fake.requests).toHaveLength(1);
  });

  it("turns error bodies into ApiError with status, code, and message", async () => {
    fakeApi({ "POST /api/v1/x": apiError(409, "head_moved", "The head moved.") });
    const err = await request("POST", "/x", { body: {} }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 409, code: "head_moved", message: "The head moved." });
  });

  it("follows next_cursor to the end of a collection", async () => {
    const fake = fakeApi({
      "GET /api/v1/repos": [json(200, { items: [{ path: "a/b" }], next_cursor: "c1" }), json(200, { items: [{ path: "a/c" }], next_cursor: null })],
    });
    const all = await getAll<{ path: string }>("/repos");
    expect(all).toEqual({ items: [{ path: "a/b" }, { path: "a/c" }], complete: true });
    expect(fake.requests[1]?.search).toContain("cursor=c1");
  });
});
