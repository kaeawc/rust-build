import { defineConfig, devices } from "@playwright/test";

// E2E against the real thing: `rb serve` built with `--features web`, so the embedded assets,
// headers (CSP, caching), /api and /ws are all exercised. RB_BIN points at that binary.
const port = Number(process.env["RB_E2E_PORT"] ?? 18090);
const rb = process.env["RB_BIN"] ?? "../target/release/rb";

export default defineConfig({
	testDir: "e2e",
	fullyParallel: true,
	forbidOnly: Boolean(process.env["CI"]),
	retries: process.env["CI"] ? 1 : 0,
	reporter: process.env["CI"] ? [["list"], ["html", { open: "never" }]] : "list",
	use: { baseURL: `http://127.0.0.1:${port}`, trace: "retain-on-failure" },
	projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
	webServer: {
		command: `${rb} serve --addr 127.0.0.1:${port}`,
		url: `http://127.0.0.1:${port}/healthz`,
		reuseExistingServer: false,
		timeout: 30_000,
	},
});
