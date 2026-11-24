/** `3725` → `1h 2m 5s`. */
export function formatUptime(totalSecs: number): string {
	const h = Math.floor(totalSecs / 3600);
	const m = Math.floor((totalSecs % 3600) / 60);
	const s = Math.floor(totalSecs % 60);
	if (h > 0) {
		return `${h}h ${m}m ${s}s`;
	}
	return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

/** Nanoseconds as milliseconds with up to two decimals. */
export function formatMs(nanos: number): string {
	return `${(nanos / 1e6).toFixed(nanos < 1e7 ? 2 : 0)} ms`;
}

/** Unix seconds as an ISO-8601 UTC timestamp. */
export function formatUnix(secs: number): string {
	return new Date(secs * 1000).toISOString().replace(".000Z", "Z");
}
