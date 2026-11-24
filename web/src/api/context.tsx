import { createContext, useContext } from "react";
import type { ReactNode } from "react";
import type { ApiClient } from "@/api/client";
import type { Timer } from "@/lib/timer";

export interface Services {
	api: ApiClient;
	timer: Timer;
	/** Opens WebSockets; tests pass a fake. */
	createSocket: (url: string) => WebSocket;
}

const ServicesContext = createContext<Services | null>(null);

export function ServicesProvider({ services, children }: { services: Services; children: ReactNode }) {
	return <ServicesContext value={services}>{children}</ServicesContext>;
}

export function useServices(): Services {
	const services = useContext(ServicesContext);
	if (services === null) {
		throw new Error("useServices outside ServicesProvider");
	}
	return services;
}
