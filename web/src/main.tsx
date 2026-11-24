import "@/index.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { FetchApiClient } from "@/api/client";
import { App } from "@/app";
import { systemTimer } from "@/lib/timer";
import { createQueryClient } from "@/router";

const root = document.getElementById("root");
if (root === null) {
	throw new Error("#root is missing from index.html");
}
createRoot(root).render(
	<StrictMode>
		<App
			services={{ api: new FetchApiClient(), timer: systemTimer, createSocket: (url) => new WebSocket(url) }}
			queryClient={createQueryClient()}
		/>
	</StrictMode>,
);
