/**
 * Scheduling goes through a Timer so tests can drive time with FakeTimer instead of waiting
 * (typescript-build's rule; ESLint forbids bare setTimeout/setInterval elsewhere).
 */
export type TimerHandle = ReturnType<typeof globalThis.setTimeout>;

export interface Timer {
	setTimeout(fn: () => void, ms: number): TimerHandle;
	clearTimeout(handle: TimerHandle): void;
	now(): number;
}

export const systemTimer: Timer = {
	setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
	clearTimeout: (handle) => {
		globalThis.clearTimeout(handle);
	},
	now: () => Date.now(),
};
