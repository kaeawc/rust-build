import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Dev loop: `make serve` (rb on 127.0.0.1:8080) + `bun run dev` (Vite on :5173). API, probes and
// the WebSocket are proxied to rb. RB_API=https://127.0.0.1:8443 for `rb serve --self-signed`
// (`secure: false` accepts the dev certificate).
const target = process.env["RB_API"] ?? "http://127.0.0.1:8080";
const upstream = { target, changeOrigin: false, secure: false };

export default defineConfig({
	plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react(), tailwindcss()],
	resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
	server: {
		proxy: {
			"/api": upstream,
			"/healthz": upstream,
			"/readyz": upstream,
			"/metrics": upstream,
			"/ws": { ...upstream, ws: true },
		},
	},
	// No production sourcemaps: everything in dist/ is embedded in the rb binary. `bun run dev` has maps.
	build: { target: "es2022", sourcemap: false },
});
