// Fast lint (no type information): correctness, React hooks, and the house style shared with
// typescript-build. Type-aware promise rules live in eslint.typechecked.config.mjs.
import js from "@eslint/js";
import stylistic from "@stylistic/eslint-plugin";
import reactHooks from "eslint-plugin-react-hooks";
import { defineConfig } from "eslint/config";
import globals from "globals";
import tseslint from "typescript-eslint";

/** typescript-build's base rules. */
export const baseRules = {
	"no-restricted-globals": [
		"error",
		{ name: "setTimeout", message: "Use Timer.setTimeout() from src/lib/timer so tests can inject FakeTimer." },
		{ name: "setInterval", message: "Schedule through Timer (src/lib/timer) so tests can inject FakeTimer." },
	],
	"no-restricted-imports": ["error", { patterns: [{ regex: "^\\.{1,2}/.*\\.(?:ts|tsx|js)$", message: "Relative imports are extensionless." }] }],
	eqeqeq: "error",
	curly: ["error", "all"],
	"prefer-const": "error",
	"no-var": "error",
	"@typescript-eslint/consistent-type-imports": ["error", { fixStyle: "separate-type-imports" }],
	"@typescript-eslint/no-unused-vars": ["error", { args: "none", caughtErrors: "none" }],
};

const style = {
	"@stylistic/quotes": ["error", "double", { avoidEscape: true }],
	"@stylistic/jsx-quotes": ["error", "prefer-single"],
	"@stylistic/semi": ["error", "always"],
	"@stylistic/indent": ["error", "tab", { SwitchCase: 1 }],
	"@stylistic/comma-dangle": ["error", "always-multiline"],
};

export default defineConfig(
	{
		ignores: ["dist/**", "coverage/**", "node_modules/**", "playwright-report/**", "test-results/**", "src/routeTree.gen.ts", "src/api/gen/**"],
	},
	js.configs.recommended,
	tseslint.configs.recommended,
	reactHooks.configs.flat.recommended,
	{
		files: ["**/*.{ts,tsx,mjs}"],
		languageOptions: { globals: { ...globals.browser, ...globals.node } },
		plugins: { "@stylistic": stylistic },
		rules: { ...baseRules, ...style },
	},
	{
		// Vendored shadcn/ui output: keep `shadcn add`/`shadcn diff` clean, so no house style.
		files: ["src/components/ui/**"],
		rules: Object.fromEntries(Object.keys(style).map((rule) => [rule, "off"])),
	},
	{
		// The Timer implementation is the one place that touches the real globals.
		files: ["src/lib/timer.ts"],
		rules: { "no-restricted-globals": "off" },
	},
);
