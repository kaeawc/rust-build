# Web frontend for rust-build: TypeScript vs Rust

Research date: 2026-11-21. Versions come from the npm registry and the crates.io API, queried that
day. Benchmark figures come from the js-framework-benchmark `results.json` at commit `1470e553da`
(2026-11-15). Claims I could not verify are marked as such.

## TL;DR

**Build `web/` as a TypeScript SPA: React 19.3, Vite 8, TanStack Router, Tailwind v4, and
shadcn/ui on Radix. Use bun, and follow typescript-build's conventions.** A small `crates/rb-web`
crate embeds the built assets in the `rb` binary behind a `web` cargo feature. Types flow one way,
Rust → TS: ts-rs generates them from the serde DTOs behind a `ts` feature. Utoipa/OpenAPI comes
later, if the API grows or outside clients appear.

Three findings decided it:

1. **Accessible components.** Radix is mature and heavily used: `@radix-ui/react-dialog` alone has
   about 102M npm downloads a week. The Rust ports are in poor shape. RustForWeb's `radix` and
   `shadcn-ui` ports were both **archived on 2026-02-02**. The best Rust option,
   DioxusLabs/components (28 Radix-style primitives), is young. The Leptos ports see a few hundred
   to a few thousand downloads per crates.io window.
2. **Rust/WASM does not load faster.** In the js-framework-benchmark, the compressed bundles are
   about the same size: React 19.2 51.4 kB, Leptos 48.8 kB, Sycamore 47.2 kB, Yew 56.8 kB, Dioxus
   114.9 kB. First paint is also close: React 228 ms, Leptos 236 ms, Dioxus 537 ms. The WASM
   frameworks are faster at bulk DOM updates (unweighted geomean vs vanilla: about 1.26 against
   React's 1.78). That gap doesn't matter for a dashboard rendering hundreds of rows.
3. **Fit with rust-build's rules and dev loop.** `leptos_axum` 0.8 and `dioxus-server` 0.7 depend
   on `tower-http` 0.6, while rust-build uses 0.7. Dioxus also uses `tokio-tungstenite` 0.28, while
   axum uses 0.29. Both clash with the `deny.toml` one-version rule. A WASM frontend also needs a
   second target with its own CI cache, plus `wasm-bindgen-cli`/`wasm-opt` pinned exactly. Vite's
   HMR updates in milliseconds; a Rust frontend recompiles. Keeping the boundary at HTTP means you
   can delete the frontend by removing two directories and a feature flag.

---

## 1. Context

- **rust-build** runs axum 0.8 on hyper-util, with HTTP/1.1, h2 and h2c, TLS on rustls/ring, and
  WebSockets at `/ws`. Its JSON API covers `/api/info`, `/api/users` (cursor pagination),
  `/api/token`, `/api/me`, `/api/admin/traffic`, `/api/admin/healthchecks`, `/healthz` and
  `/readyz`. Errors use one envelope: `{"error":{"code","message"}}`.
- **Its policies** constrain any frontend integration (see `AGENTS.md`, `deny.toml`, `Cargo.toml`):
  - `default-features = false` everywhere, and one version of every crate (`multiple-versions =
    "deny"`).
  - No aws-lc or OpenSSL.
  - Doctests off and a single e2e binary.
  - The workspace has 183 crates in `Cargo.lock` today.
  - CI allows only GitHub-owned actions plus `Swatinem/rust-cache` and `taiki-e/install-action`,
    all SHA-pinned.
- **golang-build/web**, the Go project's frontend, is the thing being replaced:
  - Vite 6, React 19, react-router-dom 7, Tailwind v4 through PostCSS, bun:test with happy-dom.
  - About 565 lines across 6 pages.
  - The Go `SPAHandler` serves it from disk, with `/assets/*` immutable and everything else
    `no-cache`.
  - Measured locally: the main JS chunk is 373.6 kB raw / 117.2 kB gzip, and most of that is the
    OpenTelemetry web SDK. The vendor chunk is 4.2 kB gzip, the CSS 4.9 kB gzip.
- **typescript-build conventions** to carry over (README, AGENTS.md, package.json, eslint configs,
  knip.json, lefthook.yml, turbo.json, bunfig.toml):
  - bun as the task runner, with every task a `package.json` script.
  - TypeScript only. No extensions in relative imports, double quotes, `jsx-quotes: prefer-single`.
  - Never call `setTimeout`/`setInterval` directly; inject a `Timer` and test with `FakeTimer`.
  - Interfaces with fakes instead of mocking libraries.
  - `bun:test`, with unit tests under 100 ms each.
  - `bun test --coverage` checked against `coverage-thresholds.json` (70/70/55/70).
  - knip and ts-prune for dead code.
  - Type-aware ESLint (`no-floating-promises`, `no-misused-promises`).
  - lefthook pre-commit and pre-push hooks, and turbo task caching.
  - Pins: TypeScript `^6.0.2`, ESLint `^10`, bun 1.3.13.

## 2. Options considered

### A. TypeScript frontend, served by axum

| Variant | Verdict |
| --- | --- |
| **Vite 8 + React 19.3 SPA + TanStack Router** | **Recommended.** Plain static output that axum can embed. Type-safe routes and search params (for example `?pageSize=&cursor=`). Automatic route code splitting through `@tanstack/router-plugin`. |
| Vite 8 + React Router 8 (declarative or data mode) | Equally viable and the most familiar. It's also the easiest port from golang-build's react-router-dom 7, since v8 [removed `react-router-dom`](https://remix.run/blog/react-router-v8) and requires React ≥19.2.7 and Vite ≥7. Its search params are less strictly typed than TanStack Router's. |
| TanStack Start 1.0 (stable [2026-10-13](https://tanstack.com/blog/tanstack-start-1-0)) | Built for SSR and server functions running on a JS server (Nitro). Its [SPA mode](https://tanstack.com/start/latest/docs/framework/react/guide/spa-mode) can prerender a shell. In front of axum it is mostly unused weight. Worth revisiting if SSR/SEO becomes a requirement, which would mean a Bun or Node sidecar. |
| React Router 8 framework mode, SSR | Same problem: needs a JS server next to axum, so `rb` stops being a single binary. |
| Next 16 `output: "export"` | Static export works, but dynamic routes need `generateStaticParams`. Next's build and conventions are heavy for a client-only app, and no server features would be used. Rejected. |

### B. Rust frontend

| Framework (Nov 2026) | Rendering | Component story | Notes |
| --- | --- | --- | --- |
| **Leptos** 0.8.23 (0.9.0-rc1 since 2026-10-31) | CSR, SSR + hydration, islands; `leptos_axum` | rust-ui/ui (Tailwind copy-paste registry, 832★, active); leptos-shadcn-ui 0.9.0 (last release 2025-09); radix-leptos-primitives 0.9.0 (2025-09, 73 recent downloads); leptix-ui 0.1.6; singlestage 0.4.1 (Basecoat-based); thaw 0.4.8 (Fluent-style) | Smallest WASM of the Rust options. Code splitting via `--split`, `#[lazy]` and `#[lazy_route]` (2025). [Binary size guide](https://book.leptos.dev/deployment/binary_size.html). `leptos_axum` 0.8 uses tower-http **0.6**; 0.9-rc1 moves to 0.7. |
| **Dioxus** 0.7.10 (0.8.0-alpha.3) | Web (CSR), fullstack SSR on axum, desktop, mobile | **DioxusLabs/components**: 28 Radix-modelled, ARIA-aware primitives installed with `dx components add` (357★, 30 open issues) | Subsecond hot-patching of Rust code, including WASM ([0.7 release](https://github.com/DioxusLabs/dioxus/releases/tag/v0.7.0)). Largest bundle in the benchmark. `dioxus-server` pulls tower-http 0.6, tokio-tungstenite 0.28 and reqwest 0.12. |
| **Yew** 0.23.0 | CSR, SSR | RustForWeb shadcn-ui and radix (**archived 2026-02-02**) | Mature but slower development. Slowest create times among the Rust frameworks in the benchmark. |
| **Sycamore** 0.9.4 (0.10.0-beta.1) | CSR, SSR | None of note | Small community (about 22k crates.io downloads per window, against 2.06M for Leptos). |

### Hybrid options

- **Rust SSR templates + htmx + Tailwind.** Templates in askama 0.16 or maud 0.27, htmx 2.0.11 for
  interactivity, and [Basecoat](https://basecoatui.com) 1.2.1 for a shadcn look in plain HTML/CSS.
  - Pros: tiny JS, server-side types, no codegen.
  - Cons: no Radix. Complex widgets (combobox, menus, popovers) use hand-rolled ARIA. Changing a
    template means a Rust recompile. Live dashboards need htmx's websocket extension.
  - This is the "no JS toolchain" choice, not the "modern frontend tools + shadcn/Radix" one.
- **Leptos + JS interop to Radix.** Radix is React components. Using it from Leptos means mounting
  React islands, so you ship both runtimes (WASM plus about 50 kB of React) and two state models.
  The two other interop routes are framework-agnostic state machines such as Zag.js (1.8M weekly
  downloads, with your own wasm-bindgen bindings), or platform primitives: `<dialog>`, the
  `popover` attribute and CSS anchor positioning. Both are workable, and neither is shadcn/Radix.
- **React islands inside askama pages.** Vite with several entry points. This adds complexity
  without a clear benefit over an SPA for an authenticated admin or dashboard UI.

## 3. Tradeoff matrix

Scale: ●●● strong · ●● adequate · ● weak. "A" means the recommended TS stack.

| Dimension | A: React/Vite/shadcn+Radix | Leptos (CSR) | Leptos (SSR/islands) | Dioxus web/fullstack | Yew / Sycamore | Rust SSR + htmx |
| --- | --- | --- | --- | --- | --- | --- |
| Initial load (compressed framework + app, js-framework-benchmark) | ●● React 51.4 kB, first paint 228 ms | ●● 48.8 kB, 236 ms | ●●● HTML first, then hydrate; islands ship less WASM | ● 114.9 kB, 537 ms | ●● 56.8 kB / 47.2 kB; 260 / 215 ms | ●●● HTML + htmx (about 16 kB gz) |
| Runtime responsiveness (geomean vs vanilla, 9 CPU tests, lower is better) | ●● 1.78 (`swap1k` 87 ms is the outlier; virtualize big lists) | ●●● 1.26 | ●●● 1.26 | ●●● 1.26 | ●● 1.47 (Yew) / ●●● 1.29 (Sycamore) | ●● each interaction is a server round trip |
| Memory at ready (MB) | ●● 1.18 | ●●● 0.64 | ●●● | ●● 0.76 | ●● 0.73 / 0.64 | ●●● |
| SSR / hydration in the `rb` binary | ● SPA only. Prerendering the shell is possible; real SSR needs a JS sidecar | ● | ●●● in-process with axum | ●●● in-process (`dioxus::serve`) | ●● | ●●● |
| Accessibility primitives | ●●● Radix: WAI-ARIA patterns, focus management, keyboard, typeahead; very widely used | ● ports young, unmaintained or archived | ● | ●● 28 first-party primitives, young | ● the RustForWeb ports these relied on are archived | ●● native elements, Basecoat; hand-rolled ARIA for complex widgets |
| Component ecosystem depth | ●●● shadcn (blocks, charts, data table, sonner, cmdk), TanStack Table/Virtual, Recharts | ● | ● | ●● | ● | ● |
| shadcn/ui + Radix integration | ●●● first-party (`shadcn init -b radix`) | ● look-alikes | ● | ●● look-alike, Radix-modelled | ● | ● Basecoat look-alike |
| Dev loop | ●●● Vite HMR with React Fast Refresh (state kept, ms); Tailwind v4 Vite plugin | ● view-macro hot reload for markup only; any logic change rebuilds wasm32 | ● rebuilds server and WASM in parallel | ●● Subsecond hot-patch; full rebuild on struct or dependency changes | ● rebuild | ● template change → recompile |
| Rust → frontend types | ●● codegen (ts-rs or OpenAPI), checked by CI diff | ●●● same structs | ●●● plus server functions | ●●● plus server functions | ●●● | ●●● |
| Testing | ●●● bun:test + happy-dom, Playwright + axe, Vitest browser mode if needed | ●● wasm-bindgen-test (headless browser) + Playwright | ●● | ●● | ●● | ●● Rust tests on HTML + Playwright |
| Fit with rust-build dependency policy | ●●● only `include_dir` (no transitive deps beyond proc-macro2/quote) | ●● CSR crates stay off the server graph | ● tower-http 0.6 duplicate on 0.8; 0.9 still RC | ● tower-http 0.6, tokio-tungstenite 0.28, reqwest 0.12 | ●● | ●● askama adds syn 2 (already skipped) |
| CI cost | ●● bun install + lint/type/test/build (seconds with Rolldown) + Playwright Chromium | ● wasm32 target + its own cache + wasm-bindgen-cli/wasm-opt/cargo-leptos | ● server compiled twice (`ssr` + `hydrate`) | ● plus `dx` | ● | ●●● Rust only |
| Hiring and maintenance | ●●● react 275M/wk, Vite 289M/wk npm downloads; LLM-friendly; shadcn ships agent skills | ● leptos about 2.06M crates.io downloads per window; breaking changes every minor (0.7 rewrite, 0.9 RC) | ● | ● 0.8 alpha "ships breaking changes" | ● | ●● plain HTML skills |
| Strip-out (delete the frontend cleanly) | ●●● HTTP boundary: delete `web/` + `crates/rb-web` + one feature | ●●● if CSR in its own workspace | ● SSR threads `ssr`/`hydrate` features and server functions through the server | ● | ●● | ● templates live inside handlers |

Notes on the evidence:

- **Benchmark caveats.** The js-framework-benchmark is a synthetic table benchmark
  ([why it is limited](https://nolanlawson.com/2024/10/13/the-greatness-and-limitations-of-the-js-framework-benchmark/)).
  The "geomean" above is my unweighted geometric mean of medians over the 9 CPU tests, not the
  site's official weighted score. The Leptos entry is v0.7.0. Sizes are the benchmark's own
  compressed transfer sizes, so the WASM figures include the JS glue.
- **Other bundle-size reports** are informal and inconsistent. One dashboard app came out at Leptos
  86 kB / Yew 180 kB / Dioxus 210 kB gzip
  ([pistack.xyz](https://www.pistack.xyz/posts/2026-08-29-dioxus-vs-yew-vs-leptos-rust-wasm-frameworks-comparison/)).
  They agree on the order (Leptos smallest, Dioxus largest), not on absolute numbers.
- **Startup cost.** WASM compiles faster per byte than JS parses, but WASM binaries are "larger
  than the JavaScript bundles for the equivalent app"
  ([Leptos book](https://book.leptos.dev/deployment/binary_size.html)). Rust's runtime advantage
  shows up in bulk DOM updates, not startup.
- **Dev-loop timings were not measured here.** No wasm32 target, cargo-leptos or trunk is installed
  locally, and I did not install toolchains for a docs-only task. The dev-loop row is about
  mechanisms: HMR vs recompile vs hot-patch.

## 4. Recommendation: the concrete stack

### Frontend (`web/`)

| Concern | Choice | Version (npm, 2026-11-21) |
| --- | --- | --- |
| Runtime and package manager | bun (`packageManager` pinned; typescript-build pins 1.3.13, bump both together) | 1.4.5 |
| Bundler and dev server | vite (Rolldown-based since [Vite 8](https://vite.dev/blog/announcing-vite8)) | 8.3.8 |
| React | react, react-dom, @vitejs/plugin-react | 19.3.1, 19.3.1, 6.3.0 |
| Router | @tanstack/react-router + @tanstack/router-plugin (`autoCodeSplitting: true`) | 1.170.46 / 1.169.0 |
| Server state | @tanstack/react-query | 5.108.2 |
| Styling | tailwindcss + @tailwindcss/vite (no PostCSS config) | 4.3.5 |
| Components | shadcn CLI, **`shadcn init -b radix`**; radix-ui (unified package); lucide-react; class-variance-authority; tw-animate-css | 4.23.0; 1.8.1; 1.77.0; 0.7.1; 1.4.0 |
| Big lists | @tanstack/react-virtual (sidesteps React's weak `swap`/`create10k` numbers) | 3.14.26 |
| TypeScript | typescript **~6.0** (6.0.3). Not 7.0.2: typescript-eslint 8.73 requires `typescript <6.1.0` | 6.0.3 |
| Lint | eslint 10 + typescript-eslint + @stylistic + eslint-plugin-react-hooks; reuse typescript-build's `baseRules` and typed config | 10.15.0 / 8.73.0 / 6.0.1 / 7.1.1 |
| Unit and component tests | bun:test + @happy-dom/global-registrator + @testing-library/react | bun / 21.0.4 / 16.3.3 |
| E2E and a11y | @playwright/test + @axe-core/playwright against a real `rb serve` | 1.65.1 / 4.14.0 |
| Dead code | knip (ignore `src/components/ui/**`, `src/api/gen/**`, `routeTree.gen.ts`) | 6.47.0 |

Why these choices:

- **Radix, not Base UI.** You asked for Radix. Since
  [July 2026](https://ui.shadcn.com/docs/changelog/2026-07-base-ui-default) shadcn defaults new
  projects to Base UI (1.6.0+, picked 2:1 on shadcn/create), so `-b radix` must be passed
  explicitly, including in any scripted init. shadcn says Radix "is not being deprecated" and that
  every update ships for both. Components are vendored source, so you can move one component at a
  time later.
- **`eslint-plugin-jsx-a11y` is left out.** Version 6.10.2 declares `eslint ≤9` as its peer and
  typescript-build is on ESLint 10. Accessibility is enforced by Radix itself plus axe in
  Playwright. Add jsx-a11y once it supports ESLint 10.
- **No React Compiler at first.** In plugin-react 6 the native (oxc) compiler is marked
  experimental, and the Babel route needs `@rolldown/plugin-babel` + `babel-plugin-react-compiler`
  1.0. The benchmark shows no speedup (1.87 vs 1.78). Add it later if profiling shows rerender
  cost.
- **No Vitest at first**, to follow typescript-build's bun:test rule. Use Vitest 5.1 browser mode
  (`@vitest/browser-playwright`) only if component tests need a real browser beyond what Playwright
  e2e covers. Radix positioning and pointer-capture behaviour is unreliable in happy-dom.
- **Turbo and lefthook are optional.** A single package in a cargo repo gains little from turbo,
  and the CI path filter gives coarse caching. rust-build has no git hooks, and a root
  `lefthook.yml` would make strip-out messier. Keep the script names identical to
  typescript-build's so either can be added later.
- **typescript-build rules that apply directly:**
  - The WebSocket reconnect/backoff, debounce and toast timers go through an injected `Timer`, so
    tests use `FakeTimer`.
  - The API client sits behind an `ApiClient` interface with a `FakeApiClient` for page tests, so
    no msw or mocking libraries.
  - `src/components/ui/**` (shadcn output) is excluded from the stylistic rules, coverage and knip.
    `shadcn add` and `shadcn diff` then stay clean, and vendored code doesn't skew thresholds.

### Directory layout

```
rust-build/
  web/                                  # the whole frontend; delete to strip it
    package.json  bun.lock  bunfig.toml # bunfig: [test] preload happy-dom, coverage → ./coverage
    index.html  vite.config.ts  components.json (shadcn, base: radix)
    tsconfig.json  tsconfig.typecheck.json
    eslint.config.mjs  eslint.typechecked.config.mjs  knip.json  coverage-thresholds.json
    playwright.config.ts
    scripts/precompress.ts              # writes .br/.gz siblings next to every text asset in dist/
    src/
      main.tsx  index.css               # @import "tailwindcss"; shadcn theme tokens
      routes/                           # TanStack file routes: __root, index, users, admin, tools
      routeTree.gen.ts                  # generated by the router plugin (committed)
      api/
        gen/                            # ts-rs output from Rust: committed, never hand-edited
        client.ts                       # ApiClient interface + fetch impl; maps the error envelope
        queries.ts                      # TanStack Query options per endpoint
      ws/socket.ts                      # typed /ws client, Timer-injected reconnect
      lib/{utils.ts,timer.ts}           # cn(); Timer/SystemTimer (ported from typescript-build)
      components/ui/                    # shadcn (vendored)
      components/                       # app components
    test/                               # bun:test suites, fakes/ (FakeApiClient, FakeTimer, FakeSocket)
    e2e/                                # Playwright specs + axe scans, run against `rb serve`
    dist/                               # build output (gitignored)
  crates/rb-web/                        # serves web/dist from inside the binary
    Cargo.toml  build.rs  src/lib.rs
```

## 5. Serving strategy (axum)

### Production: embedded in `rb`, behind a feature

- **Feature flag.** `apps/rb` gets `[features] web = ["dep:rb-web"]`, and `rb serve` mounts the SPA
  only under `#[cfg(feature = "web")]`. A plain `cargo c`, `cargo t` or CI `lint` never needs bun.
  `make dist` and the Dockerfile build `web/` first, then run `cargo build --profile dist -p rb
  --features web`.
- **Why `include_dir` 0.7.4 rather than rust-embed.**
  - `rust-embed` 8.14 pulls `sha2 0.11` (via `rust-embed-utils`), which brings `digest 0.11`.
    rust-build already has `digest 0.10` through tungstenite's `sha1 0.10`, so `cargo deny` fails
    on the duplicate. That is exactly the kind of duplicate `AGENTS.md` says to avoid when we can
    control it.
  - `include_dir` depends only on `include_dir_macros`, and that crate depends only on
    `proc-macro2` and `quote` (no syn). It embeds absolute paths with `include_bytes!`, so Cargo
    tracks edits to existing files.
  - ETags use SHA-256 from **ring**, which is already in the graph. MIME types come from a
    10-line extension match, so no `mime_guess`.
  - If you would rather not own about 150 lines, **memory-serve 2.4** is the drop-in alternative. It
    adds build-time brotli, ETag/304 and SPA fallback. It also adds `brotli`, `sha256` (on sha2
    0.10, so no digest duplicate), `mime_guess`, `walkdir` and `urlencoding`. Set
    `fallback_status` to 200, since it defaults to 404.
- **build.rs** (it must exist in any build, even without bun):

```rust
// crates/rb-web/build.rs — point RB_WEB_DIST at web/dist, or an empty dir if it isn't built.
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = manifest.join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display()); // directory: rescanned recursively
    let dir = if dist.join("index.html").exists() {
        dist.canonicalize().expect("canonical dist")
    } else {
        let empty = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("empty-dist");
        std::fs::create_dir_all(&empty).expect("create empty dist");
        empty
    };
    println!("cargo:rustc-env=RB_WEB_DIST={}", dir.display());
}
```

- **Lookup at startup** (`lib.rs`):
  - `static DIST: Dir = include_dir!("$RB_WEB_DIST");`
  - Walk `DIST` into a `HashMap<&'static str, Asset>`. Each `Asset` holds the bytes, an optional
    `.br` sibling and an optional `.gz` sibling, the content type, a `"sha256"` ETag and a
    cache-control value.
- **Response rules.** These match golang-build's `SPAHandler` and fix its gaps:

| Request | Response |
| --- | --- |
| `GET/HEAD /assets/<hash>.<ext>` found | 200, `Cache-Control: public, max-age=31536000, immutable`, ETag |
| `GET /assets/...` **not** found | **404**, not index.html. Otherwise an old tab asking for a deleted chunk gets HTML with a 200 and fails with a MIME error |
| Other existing file (favicon, robots.txt) | 200, `Cache-Control: public, max-age=3600`, ETag |
| Any other `GET/HEAD` with no file extension (client route) | `index.html`, 200, `Cache-Control: no-cache`, ETag, so redeploys show up on the next navigation |
| `If-None-Match` matches | 304, no body |
| `Accept-Encoding` includes `br`, then `gzip`, and the variant exists | serve the precompressed variant with `Content-Encoding` and `Vary: Accept-Encoding` |
| `/api/*` unmatched | JSON 404 envelope (see the router change below), never index.html |
| `web` feature on but `dist` was empty at build time | `/` returns a 503 text body "web UI not built (run `make web`)" |

- **Compression.** Precompress at build time with `web/scripts/precompress.ts`, which uses Bun's
  `node:zlib` (`brotliCompressSync` at quality 11, `gzipSync` at level 9). No Rust compression
  crate is added. tower-http's `CompressionLayer`, already on the router, skips responses that
  already carry `Content-Encoding`, so the API keeps on-the-fly gzip and assets aren't compressed
  twice.
- **Router change in `rb-server`.**
  - Move the JSON `not_found` fallback onto the nested `/api` router.
  - Expose `router_with_fallback(state, fallback: Router)`, which attaches the SPA as the top-level
    `fallback_service` *before* `.layer(...)`. Request ids, tracing, timeouts and the security
    headers then cover SPA responses too.
  - `router(state)` keeps today's behaviour: a JSON 404 everywhere.
- **Headers for HTML.**
  - Keep `nosniff`, `X-Frame-Options: DENY` and `Referrer-Policy`.
  - Add a CSP: `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
    img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'`.
  - Vite production builds emit no inline scripts. Radix's scroll lock (react-remove-scroll)
    inserts a `<style>` element, hence `'unsafe-inline'` for styles, or use a nonce. I have not
    tested this here; verify with Playwright.
  - A WASM frontend would also need `'wasm-unsafe-eval'` in `script-src`.
- **HTTP/2 helps.** `rb` already speaks h2 under TLS, so route-split chunks and `modulepreload`
  share a single connection. Splitting aggressively is cheap.

### Other ways to serve it

- **From disk.** Add tower-http's `fs` feature and use `ServeDir::new(dir).precompressed_br().
  precompressed_gzip()` with a fallback `ServeFile(index.html)`, behind `rb serve --web-dir`. tokio-util,
  percent-encoding and httpdate are already in `Cargo.lock`. Cache headers still need a
  per-path layer, and `ServeDir`'s fallback also answers missing `/assets/*`, so wrap it. Useful
  when ops wants to swap UI builds without a new binary.
- **From a CDN.** Upload `web/dist/assets` to object storage with immutable caching, and serve only
  `index.html` from axum or the CDN. `/api` and `/ws` then live on another origin, which means CORS
  and `connect-src` changes. Not worth it at this scale.

### Development: Vite dev server proxying to `rb`

Run `make serve` (`rb serve` on 127.0.0.1:8080) and `cd web && bun run dev` (Vite on :5173) side
by side. You browse :5173: HMR and Fast Refresh come from Vite, and the API and WebSocket requests
are proxied through to `rb`:

```ts
// web/vite.config.ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import path from "node:path";

// RB_API=https://127.0.0.1:8443 when `rb serve --self-signed` (secure: false accepts the dev cert).
const target = process.env["RB_API"] ?? "http://127.0.0.1:8080";
const upstream = { target, changeOrigin: false, secure: false };

export default defineConfig({
	plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react(), tailwindcss()],
	resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
	server: {
		proxy: {
			"/api": upstream,
			"/healthz": upstream,
			"/readyz": upstream,
			"/ws": { ...upstream, ws: true },
		},
	},
	build: { target: "es2022", sourcemap: true },
});
```

- **Rust-only edits.** `bacon` keeps `rb` rebuilding, and the Vite tab just refetches.
- **Checking the embedded build.** `bun run build && cargo run -p rb --features web -- serve`. The
  build script re-runs when `web/dist` changes.
- **Optional extra.** Have `rb serve --dev-proxy http://127.0.0.1:5173` reverse-proxy unknown
  paths to Vite, so the dev URL is the real origin (TLS, h2). It isn't needed at first, and the
  hyper-util client is already in the graph if it's wanted.

## 6. How types flow from Rust to TypeScript

**Now: ts-rs 12.0.1, optional, from the existing serde DTOs.**

- **Mark each DTO.** Add a `ts` feature to `rb-core` and `rb-server` (`ts = ["dep:ts-rs",
  "rb-core/ts"]`). Mark DTOs with
  `#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]`. The DTOs are `User`, `NewUser`,
  `Page<T>` (generic), `Info`, `TrafficStat`, `ProbeResult`, `TokenRequest`, plus a new
  serializable `ErrorEnvelope { error: ErrorDetail { code, message } }`, which replaces today's
  `json!` in `ApiError`. Typed WebSocket messages, when added, use serde-tagged enums, which ts-rs
  renders as discriminated unions.
- **serde attributes carry over.** ts-rs's default `serde-compat` reads `rename_all = "camelCase"`,
  `tag`, `skip_serializing_if` and similar, so `nextCursor` and `hasMore` come out matching the
  wire format.
- **Generate with `make types`:**
  `TS_RS_EXPORT_DIR=$(CURDIR)/web/src/api/gen TS_RS_LARGE_INT=number cargo test -p rb-server -p rb-core --features ts export_bindings`.
  - `TS_RS_LARGE_INT=number` matters. ts-rs maps `i64`/`u64` (`User.id`, `uptimeSecs`,
    `TrafficStat.*`) to `bigint` by default, but serde_json sends JSON numbers.
  - Values above 2^53 would lose precision; serialize those as strings in Rust if they can occur.
- **Normal builds skip it.** `cargo t` and CI `test` don't enable `ts`, so ts-rs is never compiled
  in the default graph and the export tests don't run. Feature unification is untouched for
  everyday builds.
- **CI guards against drift.** The `web` job runs `make types && git diff --exit-code
  web/src/api/gen`, so a Rust DTO change that isn't regenerated fails CI.
- **Endpoints stay hand-mapped.** ts-rs types the bodies, not the routes. `web/src/api/client.ts`
  maps routes to types for the nine or so routes, and Playwright e2e against a real `rb` catches route
  drift.

**Later, when there are outside API consumers or many endpoints: OpenAPI.**

- `utoipa` 6.0.0 + `utoipa-axum` 0.3.0 (`OpenApiRouter::routes(routes!(..)).split_for_parts()`)
  produce the spec from the handlers. `openapi-typescript` 7.13 + `openapi-fetch` 0.17 then give a
  typed client with routes, params and bodies. Orval 8.48 and `@hey-api/openapi-ts` 0.99 are the
  alternatives that also generate TanStack Query hooks.
- The cost: utoipa's derive and proc macros on every handler and schema, a heavier compile, and
  annotation upkeep. In return you get Swagger, non-TS clients and typed endpoints.
- **aide** 0.15.1 is the axum-native alternative (0.16 is in alpha).
- **Rejected for now:** `specta` (2.0.0-rc.28, still a release candidate) and `typeshare` 1.0.5
  (a CLI that parses Rust source; good if Swift or Kotlin clients appear, but its maintenance
  status is reported as unclear).

A Rust frontend would share the structs directly, which is its real advantage. It requires moving
DTOs into a crate that compiles for `wasm32-unknown-unknown`, free of axum and tokio. With ts-rs
behind a feature plus the CI diff, option A gets most of the safety for very little machinery.

## 7. Testing

| Layer | A (recommended) | B (Rust frontend) |
| --- | --- | --- |
| Logic, hooks, API client | bun:test with `FakeApiClient`/`FakeTimer`; target under 100 ms per test | plain `cargo test` for non-DOM logic |
| Components | bun:test + happy-dom + Testing Library for rendering and state. Radix popper and pointer-capture interactions go to Playwright | `wasm-bindgen-test` 0.3.80 in headless Chrome or Firefox (needs a webdriver in CI) |
| E2E | Playwright 1.65 against `rb serve` built with `--features web` (real embedding, real headers, real `/ws`) | same Playwright suite |
| Accessibility | `@axe-core/playwright` scan on every route, plus keyboard-path specs (dialog focus trap, menu typeahead) | same; more is at stake because the primitives are less proven |
| Coverage gate | `bun test --coverage` + typescript-build's `coverage-thresholds.json` script | cargo-llvm-cov does not cover wasm32 well (not verified) |
| Rust side | `rb-web` unit tests with a tiny fixture dist, covering 304, br/gz negotiation, `/assets` 404, SPA fallback, `/api` JSON 404. One e2e case in `apps/rb/tests/e2e.rs` | — |

## 8. CI

Add `.github/workflows/web.yml`, scoped like `desktop.yml`. It runs when `web/**`, `crates/rb-web/**`,
`crates/rb-server/src/**` or `crates/rb-core/src/**` change.

- **`web` job** (ubuntu, about 1–2 minutes warm):
  - install bun, then `bun install --frozen-lockfile` with `~/.bun/install/cache` cached by
    `actions/cache`;
  - `lint`, `lint:types`, `typecheck`, `test:coverage` + `coverage:check`, `dead-code`, `build`;
  - `make types` + the git diff check.
  - Rolldown makes `vite build` take seconds.
- **`e2e` job:** download the `rb` artifact built with `--features web`, or build it with its own
  rust-cache key `web-release`. Then `bunx playwright install --with-deps chromium` (cache
  `~/.cache/ms-playwright`) and `bun run e2e`.
- **Installing bun.** rust-build's action allowlist doesn't include `oven-sh/setup-bun`. Either add
  it to the policy, SHA-pinned and tracked by Dependabot, or use the GitHub-owned
  `actions/setup-node` with `npm i -g bun@1.4.5`.
- **Existing jobs are unchanged.** `fmt`, `lint`, `deny`, `test` and `build` don't enable `web` or
  `ts`.
- **Release.** The `build` job, or a release workflow, builds `web/` first, and the Dockerfile
  gains an `oven/bun` stage that produces `web/dist` before the cargo stage.

Option B would instead add:

- `rustup target add wasm32-unknown-unknown` and a second rust-cache key (WASM target dirs are
  large);
- `wasm-bindgen-cli` pinned to exactly the `wasm-bindgen` crate version (0.2.130 today; a mismatch
  fails the build), binaryen `wasm-opt`, and `cargo-leptos` 0.3.12 or `dx` 0.7.10;
- for SSR, a second full server compile with the `ssr` feature next to the `hydrate` WASM build;
- `cargo deny` skips for tower-http 0.6 (Leptos 0.8 and Dioxus) and tokio-tungstenite 0.28
  (Dioxus).

## 9. Strip-out

To remove the web frontend:

1. `rm -rf web crates/rb-web`.
2. In `apps/rb/Cargo.toml`, delete the `web` feature and `rb-web` dependency lines. In `rb serve`,
   delete the `#[cfg(feature = "web")]` block that calls `router_with_fallback`.
3. Delete the `ts` features and `cfg_attr(feature = "ts", …)` lines. `rg 'feature = "ts"'` finds
   them all. They are inert if left in place.
4. Delete `.github/workflows/web.yml`, the bun stage in `ci/Dockerfile`, and the `web`/`types`
   Makefile targets.

Nothing in the default build depends on bun, and the Rust API, tests and the `deny` policy don't
change. The JS sidecar SSR options (Start, React Router framework mode), Leptos/Dioxus SSR and
askama/htmx fail this test: each one ends up threaded through the server's crates, features or
handlers.

## 10. When to revisit

- **Choose Leptos** (0.9 once stable, which aligns with tower-http 0.7) if the team is Rust-only,
  shared structs and server functions matter more than component depth, and you accept building or
  hardening your own accessible primitives. Choose Dioxus if desktop and mobile from one codebase
  becomes a goal; it has the most credible Radix-style primitives in Rust.
- **Choose TanStack Start or a framework-mode router with a Bun sidecar** if public, SEO-relevant
  pages appear that need SSR.
- **Move from ts-rs to utoipa/OpenAPI** when a second consumer (mobile, partners) appears or there
  are more than about 20 endpoints.
- **Move to TypeScript 7** when typescript-eslint's peer range allows it.

## Sources

Versions and download counts: npm registry (`registry.npmjs.org/<pkg>/latest`,
`api.npmjs.org/downloads/point/last-week/<pkg>`) and crates.io API (`crates.io/api/v1/crates/<name>`,
`…/<version>/dependencies`), queried 2026-11-21. Local files: rust-build `README.md`, `AGENTS.md`,
`Cargo.toml`, `deny.toml`, `crates/rb-server/src/*.rs`, `.github/workflows/*.yml`;
typescript-build `README.md`, `AGENTS.md`, `package.json`, `build.ts`, `turbo.json`, `lefthook.yml`,
`eslint*.mjs`, `knip.json`, `bunfig.toml`, `coverage-thresholds.json`; golang-build
`web/package.json`, `web/vite.config.ts`, `web/dist`, `internal/handlers/spa.go`.

- js-framework-benchmark: https://github.com/krausest/js-framework-benchmark (`webdriver-ts/results.json`, commit 1470e553da, 2026-11-15)
- Benchmark limitations: https://nolanlawson.com/2024/10/13/the-greatness-and-limitations-of-the-js-framework-benchmark/
- Vite 8 announcement: https://vite.dev/blog/announcing-vite8
- React 19.3: https://react.dev/blog/2026/09/09/react-19-3
- React Router v8: https://remix.run/blog/react-router-v8
- TanStack Start 1.0: https://tanstack.com/blog/tanstack-start-1-0
- TanStack Start SPA mode: https://tanstack.com/start/latest/docs/framework/react/guide/spa-mode
- Tailwind CSS v4.3: https://tailwindcss.com/blog/tailwindcss-v4-3
- shadcn/ui changelog: https://ui.shadcn.com/docs/changelog
- shadcn/ui, Base UI as default (`-b radix`): https://ui.shadcn.com/docs/changelog/2026-07-base-ui-default
- shadcn/ui Vite install: https://ui.shadcn.com/docs/installation/vite
- Radix accessibility: https://www.radix-ui.com/primitives/docs/overview/accessibility
- Vitest 4 browser mode: https://www.infoq.com/news/2025/12/vitest-4-browser-mode
- `@vitejs/plugin-react` README (React Compiler options): https://www.npmjs.com/package/@vitejs/plugin-react
- Leptos releases: https://github.com/leptos-rs/leptos/releases
- Leptos binary size and code splitting: https://book.leptos.dev/deployment/binary_size.html
- Dioxus 0.7 release notes: https://github.com/DioxusLabs/dioxus/releases/tag/v0.7.0
- DioxusLabs/components: https://github.com/DioxusLabs/components
- RustForWeb/radix (archived 2026-02-02): https://github.com/RustForWeb/radix
- RustForWeb/shadcn-ui (archived 2026-02-02): https://github.com/RustForWeb/shadcn-ui
- rust-ui registry for Leptos: https://github.com/rust-ui/ui
- leptos-shadcn-ui: https://docs.rs/leptos-shadcn-ui
- radix-leptos-primitives: https://docs.rs/radix-leptos-primitives
- singlestage: https://docs.rs/crate/singlestage/latest
- Basecoat UI: https://basecoatui.com
- Rust WASM bundle comparison (informal): https://www.pistack.xyz/posts/2026-08-29-dioxus-vs-yew-vs-leptos-rust-wasm-frameworks-comparison/
- Rust WASM framework comparison: https://reintech.io/blog/leptos-vs-yew-vs-dioxus-rust-frontend-framework-comparison-2026
- ts-rs: https://docs.rs/ts-rs/latest/ts_rs/
- rust-embed (8.14.0; `allow_missing`, debug reads from disk): https://docs.rs/crate/rust-embed/latest
- include_dir: https://docs.rs/include_dir
- memory-serve: https://docs.rs/crate/memory-serve/latest
- utoipa-axum: https://docs.rs/utoipa-axum
- openapi-typescript / openapi-fetch: https://github.com/openapi-ts/openapi-typescript
- specta: https://github.com/specta-rs/specta
- typeshare (1Password): https://github.com/1Password/typeshare
