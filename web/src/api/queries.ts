import { queryOptions } from "@tanstack/react-query";
import type { ApiClient, UsersQuery } from "@/api/client";

/** How often live views (dashboard, admin) refresh. */
export const LIVE_REFRESH_MS = 5000;

export const queries = {
	info: (api: ApiClient) => queryOptions({ queryKey: ["info"], queryFn: () => api.info(), refetchInterval: LIVE_REFRESH_MS }),
	health: (api: ApiClient) => queryOptions({ queryKey: ["probe", "health"], queryFn: () => api.health(), refetchInterval: LIVE_REFRESH_MS }),
	ready: (api: ApiClient) => queryOptions({ queryKey: ["probe", "ready"], queryFn: () => api.ready(), refetchInterval: LIVE_REFRESH_MS }),
	users: (api: ApiClient, query: UsersQuery) => queryOptions({ queryKey: ["users", query], queryFn: () => api.listUsers(query) }),
	traffic: (api: ApiClient) => queryOptions({ queryKey: ["admin", "traffic"], queryFn: () => api.traffic(), refetchInterval: LIVE_REFRESH_MS }),
	healthchecks: (api: ApiClient) =>
		queryOptions({ queryKey: ["admin", "healthchecks"], queryFn: () => api.healthchecks(), refetchInterval: LIVE_REFRESH_MS }),
};
