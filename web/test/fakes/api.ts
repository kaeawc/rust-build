import { ApiError } from "@/api/client";
import type { ApiClient, ProbeResponse, UsersQuery } from "@/api/client";
import type { Info } from "@/api/gen/Info";
import type { NewUser } from "@/api/gen/NewUser";
import type { Page } from "@/api/gen/Page";
import type { ProbeResult } from "@/api/gen/ProbeResult";
import type { StandardClaims } from "@/api/gen/StandardClaims";
import type { TokenRequest } from "@/api/gen/TokenRequest";
import type { TokenResponse } from "@/api/gen/TokenResponse";
import type { TrafficStat } from "@/api/gen/TrafficStat";
import type { User } from "@/api/gen/User";

/** An in-memory ApiClient with the server's pagination and validation rules. */
export class FakeApiClient implements ApiClient {
	users: User[] = [
		{ id: 1, name: "Ada" },
		{ id: 2, name: "Grace" },
		{ id: 3, name: "Linus" },
	];
	traffics: TrafficStat[] = [];
	probes: ProbeResult[] = [{ name: "store", healthy: true, latencyNs: 12_000 }];
	ready_: ProbeResponse = { httpStatus: 200, body: { status: "ok", probes: this.probes } };
	failInfo: ApiError | null = null;
	calls: string[] = [];

	info(): Promise<Info> {
		this.calls.push("info");
		if (this.failInfo) {
			return Promise.reject(this.failInfo);
		}
		return Promise.resolve({ httpVersion: "HTTP/2.0", peer: "127.0.0.1:5000", uptimeSecs: 3725, version: "9.9.9" });
	}

	health(): Promise<ProbeResponse> {
		return Promise.resolve({ httpStatus: 200, body: { status: "ok" } });
	}

	ready(): Promise<ProbeResponse> {
		return Promise.resolve(this.ready_);
	}

	listUsers(query: UsersQuery): Promise<Page<User>> {
		this.calls.push(`listUsers ${JSON.stringify(query)}`);
		const size = query.pageSize ?? 20;
		const after = query.cursor === undefined ? 0 : Number(query.cursor);
		const rest = this.users.filter((u) => u.id > after);
		const items = rest.slice(0, size);
		const hasMore = rest.length > size;
		return Promise.resolve({ items, hasMore, nextCursor: hasMore ? String(items.at(-1)?.id) : null });
	}

	createUser(user: NewUser): Promise<User> {
		if (user.name.trim() === "") {
			return Promise.reject(new ApiError(400, "bad_request", "name must be 1-100 characters"));
		}
		const created = { id: this.users.length + 1, name: user.name.trim() };
		this.users.push(created);
		return Promise.resolve(created);
	}

	issueToken(request: TokenRequest): Promise<TokenResponse> {
		return Promise.resolve({ token: `token-for-${request.sub}`, expiresAt: 1_700_000_000 + (request.ttlSecs ?? 3600) });
	}

	me(token: string): Promise<StandardClaims> {
		if (!token.startsWith("token-for-")) {
			return Promise.reject(new ApiError(401, "unauthorized", "bad signature"));
		}
		return Promise.resolve({ sub: token.slice("token-for-".length), iss: "rust-build" });
	}

	traffic(): Promise<TrafficStat[]> {
		return Promise.resolve(this.traffics);
	}

	healthchecks(): Promise<ProbeResult[]> {
		return Promise.resolve(this.probes);
	}
}
