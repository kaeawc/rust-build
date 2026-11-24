// Writes .br and .gz siblings next to every compressible file in dist/, for crates/rb-web to
// serve by Accept-Encoding. A variant is kept only if it is smaller than the original.
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const COMPRESSIBLE = /\.(?:html|js|mjs|css|svg|json|txt|xml|webmanifest|map)$/;
const MIN_BYTES = 1024;

function* files(dir: string): Generator<string> {
	for (const name of readdirSync(dir)) {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) {
			yield* files(path);
		} else {
			yield path;
		}
	}
}

export function precompress(dist: string): { written: number; saved: number } {
	let written = 0;
	let saved = 0;
	for (const path of files(dist)) {
		if (!COMPRESSIBLE.test(path)) {
			continue;
		}
		const raw = readFileSync(path);
		if (raw.length < MIN_BYTES) {
			continue;
		}
		const variants: [string, Buffer][] = [
			[".br", brotliCompressSync(raw, { params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: raw.length } })],
			[".gz", gzipSync(raw, { level: 9 })],
		];
		for (const [ext, body] of variants) {
			if (body.length < raw.length) {
				writeFileSync(path + ext, body);
				written += 1;
				saved += raw.length - body.length;
			}
		}
	}
	return { written, saved };
}

if (import.meta.main) {
	const { written, saved } = precompress(join(import.meta.dir, "..", "dist"));
	console.log(`precompress: ${written} variants, ${(saved / 1024).toFixed(0)} KiB saved per full download`);
}
