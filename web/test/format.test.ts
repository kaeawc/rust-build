import { describe, expect, test } from "bun:test";
import { formatMs, formatUnix, formatUptime } from "@/lib/format";
import { systemTimer } from "@/lib/timer";
import { parseUsersSearch } from "@/routes/users";
import { percent, totals } from "../scripts/check-coverage-thresholds";

describe("formatting", () => {
	test("uptime, latency and timestamps", () => {
		expect([5, 65, 3725].map(formatUptime)).toEqual(["5s", "1m 5s", "1h 2m 5s"]);
		expect(formatMs(12_000)).toBe("0.01 ms");
		expect(formatMs(25_000_000)).toBe("25 ms");
		expect(formatUnix(1_700_000_000)).toBe("2023-11-14T22:13:20Z");
	});

	test("the system timer schedules and cancels", () => {
		const handle = systemTimer.setTimeout(() => {
			throw new Error("cancelled timers never fire");
		}, 10_000);
		systemTimer.clearTimeout(handle);
		expect(systemTimer.now()).toBeGreaterThan(0);
	});
});

describe("users search params", () => {
	test("keeps valid values only", () => {
		expect(parseUsersSearch({ pageSize: "50", cursor: "abc" })).toEqual({ pageSize: 50, cursor: "abc" });
		expect(parseUsersSearch({ pageSize: 0, cursor: "" })).toEqual({});
		expect(parseUsersSearch({ pageSize: 101 })).toEqual({});
		expect(parseUsersSearch({ pageSize: 2.5, cursor: 7 })).toEqual({});
	});
});

describe("coverage gate", () => {
	test("sums lcov totals, skipping vendored and generated files", () => {
		const lcov = ["SF:src/a.ts", "FNF:4", "FNH:3", "LF:10", "LH:8", "end_of_record", "SF:src/components/ui/button.tsx", "FNF:9", "FNH:0", "LF:50", "LH:0", "end_of_record"].join("\n");
		const t = totals(lcov);
		expect(t).toEqual({ lines: [8, 10], functions: [3, 4] });
		expect(percent(t.lines)).toBe(80);
		expect(percent([0, 0])).toBe(100);
	});
});
