import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import type { RouterHistory } from "@tanstack/react-router";
import { useState } from "react";
import { ServicesProvider } from "@/api/context";
import type { Services } from "@/api/context";
import { createAppRouter } from "@/router";

export function App({ services, queryClient, history }: { services: Services; queryClient: QueryClient; history?: RouterHistory }) {
	const [router] = useState(() => createAppRouter(queryClient, history));
	return (
		<ServicesProvider services={services}>
			<QueryClientProvider client={queryClient}>
				<RouterProvider router={router} />
			</QueryClientProvider>
		</ServicesProvider>
	);
}
