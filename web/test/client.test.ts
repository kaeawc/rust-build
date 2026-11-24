import { describe, expect, test } from "bun:test";
import { ApiError, FetchApiClient } from "@/api/client";

type Call = { url: string; init: RequestInit | undefined };

function fakeFetch(responses: Response[]) {
	const calls: Call[] = [];
	const fetchFn = (url: string, init?: RequestInit) => {
		calls.push({ url, init });
		const next = responses.shift();
		return next ? Promise.resolve(next) : Promise.reject(new Error("no response queued"));
	};
	return { calls, fetchFn };
}

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

describe("FetchApiClient", () => {
	test("builds the users query string and parses pages", async () => {
		const { calls, fetchFn } = fakeFetch([json({ items: [{ id: 1, name: "Ada" }], nextCursor: null, hasMore: false }), json({ items: [], nextCursor: null, hasMore: false })]);
		const api = new FetchApiClient(fetchFn);
		const page = await api.listUsers({ pageSize: 5, cursor: "abc=" });
		expect(page.items).toEqual([{ id: 1, name: "Ada" }]);
		expect(calls[0]?.url).toBe("/api/users?pageSize=5&cursor=abc%3D");
		await api.listUsers({});
		expect(calls[1]?.url).toBe("/api/users");
	});

	test("maps the error envelope to ApiError", async () => {
		const { fetchFn } = fakeFetch([json({ error: { code: "bad_request", message: "name must be 1-100 characters" } }, 400)]);
		const err = await new FetchApiClient(fetchFn).createUser({ name: "" }).catch((e: unknown) => e);
		expect(err).toBeInstanceOf(ApiError);
		expect(err).toMatchObject({ status: 400, code: "bad_request", message: "name must be 1-100 characters" });
	});

	test("falls back to the status line when the body is not an envelope", async () => {
		const { fetchFn } = fakeFetch([new Response("upstream down", { status: 502, statusText: "Bad Gateway" }), json({ nope: 1 }, 500)]);
		const api = new FetchApiClient(fetchFn);
		expect(await api.info().catch((e: unknown) => e)).toMatchObject({ status: 502, code: "http_error", message: "502 Bad Gateway" });
		expect(await api.traffic().catch((e: unknown) => e)).toMatchObject({ status: 500, code: "http_error" });
	});

	test("probes keep 503 bodies and fail on other errors", async () => {
		const { fetchFn } = fakeFetch([json({ status: "shutting_down" }, 503), json({ status: "ok", probes: [] }), new Response("", { status: 404 })]);
		const api = new FetchApiClient(fetchFn);
		expect(await api.ready()).toEqual({ httpStatus: 503, body: { status: "shutting_down" } });
		expect(await api.health()).toEqual({ httpStatus: 200, body: { status: "ok", probes: [] } });
		expect(api.ready()).rejects.toBeInstanceOf(ApiError);
	});

	test("sends JSON bodies and bearer tokens", async () => {
		const { calls, fetchFn } = fakeFetch([json({ token: "t", expiresAt: 5 }), json({ sub: "alice" }), json({ id: 9, name: "Bo" }, 201), json([])]);
		const api = new FetchApiClient(fetchFn, "http://rb");
		expect(await api.issueToken({ sub: "alice", ttlSecs: 60 })).toEqual({ token: "t", expiresAt: 5 });
		expect(calls[0]).toMatchObject({ url: "http://rb/api/token", init: { method: "POST", body: '{"sub":"alice","ttlSecs":60}' } });
		expect(new Headers(calls[0]?.init?.headers).get("content-type")).toBe("application/json");
		expect(await api.me("t")).toEqual({ sub: "alice" });
		expect(new Headers(calls[1]?.init?.headers).get("authorization")).toBe("Bearer t");
		expect(await api.createUser({ name: "Bo" })).toEqual({ id: 9, name: "Bo" });
		expect(await api.healthchecks()).toEqual([]);
		expect(calls[3]?.url).toBe("http://rb/api/admin/healthchecks");
	});
});
