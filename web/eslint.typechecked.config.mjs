// Type-aware lint: promise safety. Slower (builds the TypeScript program), so it's its own script.
import { defineConfig } from "eslint/config";
import tseslint from "typescript-eslint";
import base from "./eslint.config.mjs";

export default defineConfig(base, {
	files: ["src/**/*.{ts,tsx}", "test/**/*.{ts,tsx}"],
	ignores: ["src/components/ui/**"],
	languageOptions: { parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname } },
	plugins: { "@typescript-eslint": tseslint.plugin },
	rules: {
		"@typescript-eslint/await-thenable": "error",
		"@typescript-eslint/no-floating-promises": "error",
		"@typescript-eslint/no-misused-promises": ["error", { checksVoidReturn: { attributes: false } }],
	},
});
