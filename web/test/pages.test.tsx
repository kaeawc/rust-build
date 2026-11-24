import { describe, expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { ApiError } from "@/api/client";
import { FakeApiClient } from "./fakes/api";
import { FakeSocket } from "./fakes/socket";
import { renderApp } from "./render";

describe("pages", () => {
	test("dashboard shows server info and probes", async () => {
		renderApp("/");
		expect(await screen.findByText("HTTP/2.0")).toBeTruthy();
		expect(screen.getByText("1h 2m 5s")).toBeTruthy();
		expect(screen.getByText("9.9.9")).toBeTruthy();
		await waitFor(() => expect(screen.getAllByText("ok")).toHaveLength(2));
		expect(screen.getByText("store")).toBeTruthy();
	});

	test("dashboard surfaces API errors", async () => {
		const api = new FakeApiClient();
		api.failInfo = new ApiError(500, "internal", "boom");
		api.ready_ = { httpStatus: 503, body: { status: "shutting_down" } };
		renderApp("/", api);
		expect(await screen.findByText("boom (internal, HTTP 500)")).toBeTruthy();
		expect(await screen.findByText("shutting_down")).toBeTruthy();
	});

	test("users paginate with the cursor from the previous page", async () => {
		const api = new FakeApiClient();
		const { history } = renderApp("/users?pageSize=2", api);
		expect(await screen.findByText("Ada")).toBeTruthy();
		expect(screen.queryByText("Linus")).toBeNull();
		fireEvent.click(await screen.findByRole("link", { name: "Next page" }));
		expect(await screen.findByText("Linus")).toBeTruthy();
		expect(history.location.search).toContain("cursor=");
		expect(screen.getByRole("link", { name: "First page" })).toBeTruthy();
		expect(api.calls).toContain('listUsers {"pageSize":2,"cursor":"2"}');
	});

	test("creating a user closes the dialog and refreshes the list", async () => {
		const api = new FakeApiClient();
		renderApp("/users", api);
		await screen.findByText("Ada");
		fireEvent.click(screen.getByRole("button", { name: "New user" }));
		const dialog = await screen.findByRole("dialog");
		fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "  Barbara " } });
		fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
		expect(await screen.findByText("Barbara")).toBeTruthy();
		await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
		expect(api.users.at(-1)).toEqual({ id: 4, name: "Barbara" });
	});

	test("tokens are issued and verified", async () => {
		renderApp("/tokens");
		fireEvent.change(await screen.findByLabelText("Subject"), { target: { value: "bob" } });
		fireEvent.click(screen.getByRole("button", { name: "Issue token" }));
		expect((await screen.findByTestId("token")).textContent).toBe("token-for-bob");
		fireEvent.click(screen.getByRole("button", { name: "Call /api/me" }));
		expect((await screen.findByTestId("claims")).textContent).toContain('"sub": "bob"');
	});

	test("admin lists traffic and health checks", async () => {
		const api = new FakeApiClient();
		api.traffics = [{ method: "GET", path: "/api/users", count: 4, status2xx: 3, status4xx: 1, status5xx: 0, lastStatus: 200, lastNanos: 1, totalNanos: 8_000_000 }];
		api.probes = [{ name: "store", healthy: false, latencyNs: 1, err: "timed out" }];
		renderApp("/admin", api);
		expect(await screen.findByText("GET /api/users")).toBeTruthy();
		expect(screen.getByText("2.00 ms")).toBeTruthy();
		expect(await screen.findByText("timed out")).toBeTruthy();
	});

	test("websocket console sends and shows echoes", async () => {
		FakeSocket.reset();
		renderApp("/websocket");
		const send = await screen.findByRole("button", { name: "Send" });
		const socket = FakeSocket.last();
		expect(socket.url).toBe("ws://localhost:8080/ws");
		act(() => socket.open());
		await waitFor(() => expect((send as HTMLButtonElement).disabled).toBe(false));
		fireEvent.change(screen.getByLabelText("Message"), { target: { value: "ping" } });
		fireEvent.click(send);
		act(() => socket.receive("ping"));
		const log = screen.getByRole("list", { name: "Messages" });
		await waitFor(() => expect(within(log).getAllByText("ping")).toHaveLength(2));
		expect(socket.sent).toEqual(["ping"]);
	});

	test("unknown routes render the not-found page", async () => {
		renderApp("/nope");
		expect(await screen.findByText("Not found")).toBeTruthy();
	});
});
