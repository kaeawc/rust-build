import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const routes = [
	{ path: "/", heading: "Dashboard" },
	{ path: "/users", heading: "Users" },
	{ path: "/tokens", heading: "Tokens" },
	{ path: "/admin", heading: "Admin" },
	{ path: "/websocket", heading: "WebSocket" },
];

/** Records CSP violations the page reports, from before any script runs. */
async function watchCsp(page: Page): Promise<() => Promise<string[]>> {
	await page.addInitScript(() => {
		const w = window as unknown as { cspViolations: string[] };
		w.cspViolations = [];
		document.addEventListener("securitypolicyviolation", (e) => {
			w.cspViolations.push(`${e.effectiveDirective}: ${e.blockedURI || "inline"}`);
		});
	});
	return () => page.evaluate(() => (window as unknown as { cspViolations: string[] }).cspViolations);
}

for (const { path, heading } of routes) {
	test(`${path} renders, has no axe violations and no CSP violations`, async ({ page }) => {
		const violations = await watchCsp(page);
		const errors: string[] = [];
		page.on("pageerror", (e) => errors.push(e.message));
		const res = await page.goto(path);
		expect(res?.headers()["content-security-policy"]).toContain("default-src 'self'");
		expect(res?.headers()["cache-control"]).toBe("no-cache");
		await expect(page.getByRole("heading", { level: 1, name: heading })).toBeVisible();
		await page.waitForLoadState("networkidle");
		const axe = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"]).analyze();
		expect(axe.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
		expect(await violations()).toEqual([]);
		expect(errors).toEqual([]);
	});
}

test("create a user through the dialog: focus is trapped and returned, CSP holds", async ({ page }) => {
	const violations = await watchCsp(page);
	await page.goto("/users");
	const trigger = page.getByRole("button", { name: "New user" });
	await trigger.click();
	const dialog = page.getByRole("dialog", { name: "New user" });
	await expect(dialog).toBeVisible();
	await expect(dialog.getByLabel("Name")).toBeFocused();
	// Tab cycles inside the dialog (Radix focus trap).
	for (let i = 0; i < 4; i++) {
		await page.keyboard.press("Tab");
		expect(await dialog.evaluate((d) => d.contains(document.activeElement))).toBe(true);
	}
	const axe = await new AxeBuilder({ page }).include("[role=dialog]").analyze();
	expect(axe.violations.map((v) => v.id)).toEqual([]);
	await page.keyboard.press("Escape");
	await expect(dialog).toBeHidden();
	await expect(trigger).toBeFocused();

	const name = `e2e-${Date.now()}`;
	await trigger.click();
	await dialog.getByLabel("Name").fill(name);
	await dialog.getByRole("button", { name: "Create" }).click();
	await expect(dialog).toBeHidden();
	await page.getByRole("link", { name: "50" }).click();
	await expect(page.getByRole("cell", { name, exact: true })).toBeVisible();
	expect(await violations()).toEqual([]);
});

// The open question from docs/research/web-frontend.md: does Radix's scroll lock need
// `style-src 'unsafe-inline'`? Serve the page with a strict style-src and open a dialog. If this
// starts failing (no violation), Radix stopped injecting styles: drop 'unsafe-inline' from
// rb_web::CSP.
test("Radix scroll lock needs style-src 'unsafe-inline' (strict CSP is violated)", async ({ page }) => {
	await page.route("**/users", async (route) => {
		const res = await route.fetch();
		const csp = (res.headers()["content-security-policy"] ?? "").replace("style-src 'self' 'unsafe-inline'", "style-src 'self'");
		await route.fulfill({ response: res, headers: { ...res.headers(), "content-security-policy": csp } });
	});
	const violations = await watchCsp(page);
	await page.goto("/users");
	await expect(page.getByRole("heading", { level: 1, name: "Users" })).toBeVisible();
	const before = await violations();
	await page.getByRole("button", { name: "New user" }).click();
	await expect(page.getByRole("dialog")).toBeVisible();
	const after = await violations();
	expect(after.length).toBeGreaterThan(before.length);
	expect(after.slice(before.length).every((v) => v.startsWith("style-src"))).toBe(true);
});

test("tokens: issue and verify", async ({ page }) => {
	await page.goto("/tokens");
	await page.getByLabel("Subject").fill("playwright");
	await page.getByRole("button", { name: "Issue token" }).click();
	await expect(page.getByTestId("token")).toContainText(".");
	await page.getByRole("button", { name: "Call /api/me" }).click();
	await expect(page.getByTestId("claims")).toContainText('"sub": "playwright"');
});

test("websocket echo round-trips", async ({ page }) => {
	await page.goto("/websocket");
	await expect(page.getByText("open", { exact: true })).toBeVisible();
	await page.getByLabel("Message", { exact: true }).fill("over the wire");
	await page.getByRole("button", { name: "Send" }).click();
	await expect(page.locator("[data-direction=received]")).toHaveText(/over the wire/);
});

test("deep links reload; stale assets and unknown API paths are not HTML", async ({ page, request }) => {
	await page.goto("/users?pageSize=10");
	await expect(page.getByRole("heading", { level: 1, name: "Users" })).toBeVisible();
	await expect(page.getByRole("link", { name: "10" })).toHaveAttribute("aria-current", "page");

	const stale = await request.get("/assets/index-deleted.js");
	expect(stale.status()).toBe(404);
	expect(stale.headers()["content-type"]).toContain("text/plain");
	const api = await request.get("/api/does-not-exist");
	expect(api.status()).toBe(404);
	expect(await api.json()).toEqual({ error: { code: "not_found", message: "no route" } });

	const index = await request.get("/", { headers: { "accept-encoding": "br" } });
	const html = await index.text();
	const script = /src="(\/assets\/[^"]+\.js)"/.exec(html)?.[1];
	expect(script).toBeTruthy();
	const asset = await request.get(script ?? "", { headers: { "accept-encoding": "br" } });
	expect(asset.headers()["cache-control"]).toBe("public, max-age=31536000, immutable");
	const etag = asset.headers()["etag"] ?? "";
	const again = await request.get(script ?? "", { headers: { "accept-encoding": "br", "if-none-match": etag } });
	expect(again.status()).toBe(304);
});
