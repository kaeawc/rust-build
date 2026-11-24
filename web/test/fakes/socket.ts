type Listener = (event: { data?: unknown }) => void;

/** Just enough of WebSocket for EchoSocket; tests drive it with `open`, `receive` and `drop`. */
export class FakeSocket {
	static instances: FakeSocket[] = [];
	readonly url: string;
	readyState = 0;
	readonly sent: string[] = [];
	private readonly listeners = new Map<string, Listener[]>();

	constructor(url: string) {
		this.url = url;
		FakeSocket.instances.push(this);
	}

	static create = (url: string): WebSocket => new FakeSocket(url) as unknown as WebSocket;

	static reset(): void {
		FakeSocket.instances = [];
	}

	static last(): FakeSocket {
		const s = FakeSocket.instances.at(-1);
		if (!s) {
			throw new Error("no socket opened");
		}
		return s;
	}

	addEventListener(type: string, fn: Listener): void {
		this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
	}

	private emit(type: string, event: { data?: unknown } = {}): void {
		for (const fn of this.listeners.get(type) ?? []) {
			fn(event);
		}
	}

	send(text: string): void {
		this.sent.push(text);
	}

	close(): void {
		if (this.readyState !== 3) {
			this.readyState = 3;
			this.emit("close");
		}
	}

	open(): void {
		this.readyState = 1;
		this.emit("open");
	}

	receive(data: unknown): void {
		this.emit("message", { data });
	}

	/** The server went away. */
	drop(): void {
		this.close();
	}
}
