// Fails when coverage/lcov.info (from `bun test --coverage`) is below coverage-thresholds.json.
// Vendored shadcn components and generated code are excluded, so they neither help nor hurt.
import { readFileSync } from "node:fs";
import { join } from "node:path";

const EXCLUDED = [/^src\/components\/ui\//, /^src\/api\/gen\//, /^src\/routeTree\.gen\.ts$/, /^test\//];

interface Totals {
	lines: [hit: number, found: number];
	functions: [hit: number, found: number];
}

export function totals(lcov: string): Totals {
	const t: Totals = { lines: [0, 0], functions: [0, 0] };
	let skip = false;
	for (const line of lcov.split("\n")) {
		const [key, value = ""] = line.split(":", 2);
		const n = Number(value);
		switch (key) {
			case "SF":
				skip = EXCLUDED.some((re) => re.test(value.replace(/\\/g, "/")));
				break;
			case "LH":
				if (!skip) {
					t.lines[0] += n;
				}
				break;
			case "LF":
				if (!skip) {
					t.lines[1] += n;
				}
				break;
			case "FNH":
				if (!skip) {
					t.functions[0] += n;
				}
				break;
			case "FNF":
				if (!skip) {
					t.functions[1] += n;
				}
				break;
			default:
				break;
		}
	}
	return t;
}

export function percent([hit, found]: [number, number]): number {
	return found === 0 ? 100 : (hit / found) * 100;
}

if (import.meta.main) {
	const root = join(import.meta.dir, "..");
	const { thresholds } = JSON.parse(readFileSync(join(root, "coverage-thresholds.json"), "utf8")) as {
		thresholds: Record<keyof Totals, number>;
	};
	const t = totals(readFileSync(join(root, "coverage", "lcov.info"), "utf8"));
	let failed = false;
	for (const key of ["lines", "functions"] as const) {
		const got = percent(t[key]);
		const ok = got >= thresholds[key];
		failed ||= !ok;
		console.log(`${ok ? "ok  " : "FAIL"} ${key}: ${got.toFixed(1)}% (min ${thresholds[key]}%)`);
	}
	if (failed) {
		process.exit(1);
	}
}
