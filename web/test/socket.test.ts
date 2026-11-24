import { beforeEach, describe, expect, test } from "bun:test";
import { EchoSocket, sameOriginWsUrl } from "@/ws/socket";
import type { SocketStatus } from "@/ws/socket";
import { FakeSocket } from "./fakes/socket";
import { FakeTimer } from "./fakes/timer";

function setup() {
	const timer = new FakeTimer();
	const statuses: SocketStatus[] = [];
	const messages: string[] = [];
	const socket = new EchoSocket({
		url: "ws://rb/ws",
		timer,
		createSocket: FakeSocket.create,
		onStatus: (s) => statuses.push(s),
		onMessage: (m) => messages.push(m),
	});
	return { timer, statuses, messages, socket };
}

describe("EchoSocket", () => {
	beforeEach(() => FakeSocket.reset());

	test("connects, sends only when open, and reports messages", () => {
		const { statuses, messages, socket } = setup();
		socket.connect();
		expect(FakeSocket.last().url).toBe("ws://rb/ws");
		expect(socket.send("early")).toBe(false);
		FakeSocket.last().open();
		expect(socket.send("hi")).toBe(true);
		expect(FakeSocket.last().sent).toEqual(["hi"]);
		FakeSocket.last().receive("hi");
		FakeSocket.last().receive(new ArrayBuffer(2));
		expect(messages).toEqual(["hi", "[binary]"]);
		expect(statuses).toEqual(["connecting", "open"]);
	});

	test("reconnects with exponential backoff, reset by a successful open", () => {
		const { timer, statuses, socket } = setup();
		socket.connect();
		FakeSocket.last().drop();
		expect(timer.pendingDelays()).toEqual([500]);
		timer.advance(500);
		expect(FakeSocket.instances).toHaveLength(2);
		FakeSocket.last().drop();
		expect(timer.pendingDelays()).toEqual([1000]);
		timer.advance(1000);
		FakeSocket.last().open();
		FakeSocket.last().drop();
		expect(timer.pendingDelays()).toEqual([500]);
		expect(statuses.filter((s) => s === "closed")).toHaveLength(3);
	});

	test("backoff is capped", () => {
		const { socket } = setup();
		expect([1, 2, 3, 6, 10].map((n) => socket.backoff(n))).toEqual([500, 1000, 2000, 10_000, 10_000]);
	});

	test("close stops reconnecting, including a pending retry", () => {
		const { timer, socket } = setup();
		socket.connect();
		FakeSocket.last().drop();
		socket.close();
		expect(timer.pendingDelays()).toEqual([]);
		timer.advance(60_000);
		expect(FakeSocket.instances).toHaveLength(1);

		socket.connect();
		FakeSocket.last().open();
		socket.close();
		expect(timer.pendingDelays()).toEqual([]);
	});

	test("same-origin URLs follow the page scheme", () => {
		expect(sameOriginWsUrl({ protocol: "https:", host: "rb.example:8443" }, "/ws")).toBe("wss://rb.example:8443/ws");
		expect(sameOriginWsUrl({ protocol: "http:", host: "127.0.0.1:8080" }, "/ws")).toBe("ws://127.0.0.1:8080/ws");
	});
});
