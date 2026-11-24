import type { ErrorEnvelope } from "@/api/gen/ErrorEnvelope";
import type { HealthStatus } from "@/api/gen/HealthStatus";
import type { Info } from "@/api/gen/Info";
import type { NewUser } from "@/api/gen/NewUser";
import type { Page } from "@/api/gen/Page";
import type { ProbeResult } from "@/api/gen/ProbeResult";
import type { StandardClaims } from "@/api/gen/StandardClaims";
import type { TokenRequest } from "@/api/gen/TokenRequest";
import type { TokenResponse } from "@/api/gen/TokenResponse";
import type { TrafficStat } from "@/api/gen/TrafficStat";
import type { User } from "@/api/gen/User";

/** A non-2xx response, carrying the server's `{"error": {code, message}}` envelope when present. */
export class ApiError extends Error {
	readonly status: number;
	readonly code: string;

	constructor(status: number, code: string, message: string) {
		super(message);
		this.name = "ApiError";
		this.status = status;
		this.code = code;
	}
}

/** A probe answers with a body on 503 too, so its HTTP status travels with it. */
export interface ProbeResponse {
	httpStatus: number;
	body: HealthStatus;
}

export interface UsersQuery {
	pageSize?: number;
	cursor?: string;
}

/** Every call the UI makes. Pages take this interface, so tests pass a FakeApiClient. */
export interface ApiClient {
	info(): Promise<Info>;
	health(): Promise<ProbeResponse>;
	ready(): Promise<ProbeResponse>;
	listUsers(query: UsersQuery): Promise<Page<User>>;
	createUser(user: NewUser): Promise<User>;
	issueToken(request: TokenRequest): Promise<TokenResponse>;
	me(token: string): Promise<StandardClaims>;
	traffic(): Promise<TrafficStat[]>;
	healthchecks(): Promise<ProbeResult[]>;
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

function isEnvelope(value: unknown): value is ErrorEnvelope {
	if (typeof value !== "object" || value === null || !("error" in value)) {
		return false;
	}
	const error = (value as { error: unknown }).error;
	return typeof error === "object" && error !== null && "code" in error && "message" in error;
}

async function toError(res: Response): Promise<ApiError> {
	try {
		const body: unknown = await res.json();
		if (isEnvelope(body)) {
			return new ApiError(res.status, body.error.code, body.error.message);
		}
	} catch {
		// Not JSON: fall through to the status line.
	}
	return new ApiError(res.status, "http_error", `${res.status} ${res.statusText}`.trim());
}

/** The real client: same-origin fetch against `rb serve` (or Vite's dev proxy). */
export class FetchApiClient implements ApiClient {
	private readonly fetchFn: Fetch;
	private readonly base: string;

	constructor(fetchFn: Fetch = (input, init) => globalThis.fetch(input, init), base = "") {
		this.fetchFn = fetchFn;
		this.base = base;
	}

	private async json<T>(path: string, init?: RequestInit): Promise<T> {
		const res = await this.fetchFn(this.base + path, { ...init, headers: { accept: "application/json", ...init?.headers } });
		if (!res.ok) {
			throw await toError(res);
		}
		return (await res.json()) as T;
	}

	private async probe(path: string): Promise<ProbeResponse> {
		const res = await this.fetchFn(this.base + path, { headers: { accept: "application/json" } });
		if (!res.ok && res.status !== 503) {
			throw await toError(res);
		}
		return { httpStatus: res.status, body: (await res.json()) as HealthStatus };
	}

	private post<T>(path: string, body: unknown): Promise<T> {
		return this.json<T>(path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json" } });
	}

	info(): Promise<Info> {
		return this.json("/api/info");
	}

	health(): Promise<ProbeResponse> {
		return this.probe("/healthz");
	}

	ready(): Promise<ProbeResponse> {
		return this.probe("/readyz");
	}

	listUsers(query: UsersQuery): Promise<Page<User>> {
		const params = new URLSearchParams();
		if (query.pageSize !== undefined) {
			params.set("pageSize", String(query.pageSize));
		}
		if (query.cursor !== undefined) {
			params.set("cursor", query.cursor);
		}
		const qs = params.toString();
		return this.json(`/api/users${qs ? `?${qs}` : ""}`);
	}

	createUser(user: NewUser): Promise<User> {
		return this.post("/api/users", user);
	}

	issueToken(request: TokenRequest): Promise<TokenResponse> {
		return this.post("/api/token", request);
	}

	me(token: string): Promise<StandardClaims> {
		return this.json("/api/me", { headers: { authorization: `Bearer ${token}` } });
	}

	traffic(): Promise<TrafficStat[]> {
		return this.json("/api/admin/traffic");
	}

	healthchecks(): Promise<ProbeResult[]> {
		return this.json("/api/admin/healthchecks");
	}
}
