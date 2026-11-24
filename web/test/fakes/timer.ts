import type { Timer, TimerHandle } from "@/lib/timer";

/** A Timer that only moves when `advance` is called. */
export class FakeTimer implements Timer {
	private current = 0;
	private nextId = 1;
	private readonly pending = new Map<number, { at: number; fn: () => void }>();

	setTimeout(fn: () => void, ms: number): TimerHandle {
		const id = this.nextId++;
		this.pending.set(id, { at: this.current + ms, fn });
		return id as unknown as TimerHandle;
	}

	clearTimeout(handle: TimerHandle): void {
		this.pending.delete(handle as unknown as number);
	}

	now(): number {
		return this.current;
	}

	/** Delays of the timers still waiting, relative to now. */
	pendingDelays(): number[] {
		return [...this.pending.values()].map((p) => p.at - this.current).sort((a, b) => a - b);
	}

	advance(ms: number): void {
		const target = this.current + ms;
		for (;;) {
			const due = [...this.pending.entries()].filter(([, p]) => p.at <= target).sort((a, b) => a[1].at - b[1].at)[0];
			if (!due) {
				break;
			}
			const [id, p] = due;
			this.pending.delete(id);
			this.current = p.at;
			p.fn();
		}
		this.current = target;
	}
}
