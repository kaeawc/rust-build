import { GlobalRegistrator } from "@happy-dom/global-registrator";

GlobalRegistrator.register({ url: "http://localhost:8080/" });

// Testing Library's auto-cleanup hooks into Jest/Vitest globals; register it for bun:test.
const { afterEach } = await import("bun:test");
const { cleanup } = await import("@testing-library/react");
afterEach(() => {
	cleanup();
});
