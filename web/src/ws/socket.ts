import type { Timer, TimerHandle } from "@/lib/timer";

export type SocketStatus = "connecting" | "open" | "closed";

/** `WebSocket.OPEN`, without depending on the global (absent in some test environments). */
const OPEN = 1;

export interface EchoSocketOptions {
	url: string;
	timer: Timer;
	createSocket: (url: string) => WebSocket;
	onMessage: (text: string) => void;
	onStatus: (status: SocketStatus) => void;
	/** First reconnect delay; doubles per failure up to `maxBackoffMs`. */
	baseBackoffMs?: number;
	maxBackoffMs?: number;
}

/** `ws://` or `wss://` URL for `path` on the page's own origin. */
export function sameOriginWsUrl(location: Pick<Location, "protocol" | "host">, path: string): string {
	const scheme = location.protocol === "https:" ? "wss:" : "ws:";
	return `${scheme}//${location.host}${path}`;
}

/** A WebSocket to `/ws` that reconnects with exponential backoff until closed. */
export class EchoSocket {
	private readonly opts: Required<EchoSocketOptions>;
	private socket: WebSocket | null = null;
	private retry: TimerHandle | null = null;
	private failures = 0;
	private stopped = false;

	constructor(opts: EchoSocketOptions) {
		this.opts = { baseBackoffMs: 500, maxBackoffMs: 10_000, ...opts };
	}

	/** Delay before reconnect attempt number `failures` (1-based). */
	backoff(failures: number): number {
		return Math.min(this.opts.maxBackoffMs, this.opts.baseBackoffMs * 2 ** Math.max(0, failures - 1));
	}

	connect(): void {
		this.stopped = false;
		this.opts.onStatus("connecting");
		const socket = this.opts.createSocket(this.opts.url);
		this.socket = socket;
		socket.addEventListener("open", () => {
			this.failures = 0;
			this.opts.onStatus("open");
		});
		socket.addEventListener("message", (event: MessageEvent) => {
			this.opts.onMessage(typeof event.data === "string" ? event.data : "[binary]");
		});
		socket.addEventListener("close", () => {
			this.socket = null;
			this.opts.onStatus("closed");
			if (!this.stopped) {
				this.failures += 1;
				this.retry = this.opts.timer.setTimeout(() => {
					this.retry = null;
					this.connect();
				}, this.backoff(this.failures));
			}
		});
	}

	/** Sends if open; returns whether it did. */
	send(text: string): boolean {
		if (this.socket?.readyState !== OPEN) {
			return false;
		}
		this.socket.send(text);
		return true;
	}

	close(): void {
		this.stopped = true;
		if (this.retry !== null) {
			this.opts.timer.clearTimeout(this.retry);
			this.retry = null;
		}
		this.socket?.close();
		this.socket = null;
	}
}
