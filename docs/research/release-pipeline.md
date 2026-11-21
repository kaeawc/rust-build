# Release pipeline for rust-build

Research and proposal, 2026-11-21. Nothing here is implemented yet. The draft workflow and
scripts below are meant to be copied into `.github/workflows/release.yml` and
`scripts/release/` once the open questions at the end are settled.

## TL;DR

- **Trigger:** the owner pushes an immutable `vX.Y.Z[-pre]` tag (with `scripts/release/bump.sh`
  doing the version bump, commit and tag). `workflow_dispatch` and a nightly `schedule` run the
  same pipeline as a dry run that publishes nothing and keeps the artifacts, so desktop nightlies
  come for free and the pipeline is tested every day, not only on release day.
- **Version source:** `[workspace.package] version` in `Cargo.toml`. The tag must equal
  `v` + that version, and `desktop/Cargo.toml` must carry the same version. `rb --version` is
  checked against it in every smoke test.
- **Build:** native GitHub-hosted runners only. No `cross`, no zig and no QEMU: linux x86_64 and
  aarch64 (gnu and musl), macOS aarch64 (native) and x86_64 (cross-compiled on arm64), Windows
  x86_64 MSVC. Release builds use `--locked --profile dist` and restore no caches.
- **Outputs:** `rb-<ver>-<target>.tar.gz|zip`, a CycloneDX SBOM per archive, desktop app archives
  for 3 OSes, one sorted `SHA256SUMS`, GitHub artifact attestations (SLSA provenance for every
  file, SBOM attestation per archive), a multi-arch GHCR image `ghcr.io/kaeawc/rust-build` with
  BuildKit provenance and SBOM plus a GitHub attestation, and a Homebrew formula in
  `kaeawc/homebrew-tap` for stable tags.
- **Smoke tests:** every archive is checksum-verified, extracted on a clean runner of its own
  OS/arch (musl inside Alpine, x86_64 macOS on an Intel runner), and exercised with
  `--version`, `serve`, `get` (h1 and h2 over TLS), `loadgen` and `cert`. The image runs `/healthz`
  and `/readyz`. The desktop app is launched and has to write Rune's ready receipt.
- **Actions:** no new third-party actions. The design uses `actions/checkout`,
  `actions/upload-artifact`, `actions/download-artifact`, `actions/attest` (all GitHub-owned) and
  `taiki-e/install-action` (already allowed). Docker, GHCR login, release creation and the tap
  update use the preinstalled `docker buildx`, `gh` and `git` CLIs.

---

## 1. Survey of the owner's existing pipelines

None of the surveyed repos is Rust. spectra, krit and pyhotlint are Go, auto-mobile is
TypeScript/Bun plus Android/iOS, and android-build is Gradle. None of them uses cargo-dist,
`cross` or cargo-zigbuild. The closest analogue is krit's `zig cc` wrappers for cgo
cross-compiling.

| Repo / workflow | Trigger | Version source | Notes / changelog | Build matrix | Artifact naming | Checksums / signing | Attestation / SBOM | Registry / Homebrew | Smoke test | Hardening |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **spectra** `release.yml` (+ `scripts/dist.sh`, `release-manifest.sh`, `notarize-archives.sh`, `release-check.sh`) | `push: tags: v*`, plus `workflow_dispatch(version)`, which builds an unsigned dry run and never signs or publishes | the tag (regex `^v\d+\.\d+\.\d+(-pre)?$`), injected via `-ldflags -X main.version` | `gh release create --generate-notes --verify-tag`; `--prerelease` if the tag has `-`; no CHANGELOG | one `macos-latest` job cross-compiles darwin arm64/amd64 and linux amd64/arm64 (Go) | `spectra_${VERSION}_${os}_${arch}.tar.gz` (VERSION keeps the `v`), with `bin/`, `docs/`, `licenses/` inside | `checksums.txt` (shasum -a 256); an Ed25519-signed JSON release manifest from a `sign` job in the protected **`release` environment**; optional `codesign` if `CODESIGN_ID` is set; notarization script is manual | none | Homebrew formula *template* (`packaging/homebrew/spectra.rb`, source build), not in the tap yet | `release-manifest.sh` extracts the native archive, runs `spectra capabilities --json` and asserts the embedded version equals the tag | SHA-pinned actions; signing job uses `persist-credentials: false`, `cache: false` ("no shared cache is restored"); `concurrency: release-${ref}`, no cancel; local `release-check.sh` readiness checklist |
| **krit** `release.yml` | `push: tags: v*` | the tag (goreleaser `{{.Version}}`, `v` stripped) | `--generate-notes`; hand-kept **Keep a Changelog** `CHANGELOG.md` (`## [Unreleased]`, `## [0.2.0] - date`) | per-OS matrix (ubuntu/macos/windows-latest), goreleaser `--skip=publish` per OS; linux arm64 and musl via **zig cc** wrappers; darwin amd64 cross from arm64 | `krit_<ver>_<os>_<arch>.tar.gz`, `.zip` on Windows, `krit_<ver>_linux_musl_amd64` | combined, sorted **`checksums.txt`** built in a funnel job, plus a **GPG detached signature** (`crazy-max/ghaction-import-gpg`) | **`actions/attest-build-provenance`** on archives and jars; **syft** SBOM per archive (`anchore/sbom-action/download-syft`) | Homebrew **cask** pushed to `kaeawc/homebrew-tap` (`scripts/release/update-brew-tap.sh`, `HOMEBREW_TAP_TOKEN`), Scoop, winget, all **stable tags only** | none in the workflow | release.yml SHA-pinned (other workflows mixed); per-job artifacts with `retention-days: 1` |
| **krit** `nightly.yml` | cron `0 6 * * *` plus dispatch(`force`) | computes `v<major>.<minor+1>.0-nightly.YYYYMMDD[.N]` and pushes the tag with a PAT so release.yml fires | n/a | n/a | n/a | n/a | n/a | prerelease only | skips when there are no commits since the last nightly | needs a PAT (`GITHUB_TOKEN` tag pushes don't trigger workflows) |
| **krit** `publish-maven-central.yml`, `publish-intellij-plugin.yml` | tag push plus dispatch for recovery; the plugin uses its **own tag namespace** `intellij-plugin-v*` | tag, or dispatch input | n/a | n/a | n/a | n/a | n/a | stable-only gate (`!contains(ref_name, '-')`); dispatch defaults to the safe `user_managed` mode | n/a | concurrency per ref; fail-closed version resolution |
| **pyhotlint** `release.yml` | `push: tags: v*.*.*` | the tag | `softprops/action-gh-release` with `generate_release_notes` | native matrix: `macos-13` (amd64, a retired runner image), `macos-latest`, `ubuntu-latest`; no linux arm64 | `pyhotlint-<tag>-<os>-<arch>.tar.gz` | per-archive `.sha256` | none | none | none | unpinned `@v6`/`@v4` tags; a third-party release action |
| **auto-mobile** `prepare-release.yml` → `release.yml` | `workflow_dispatch(version)`. It bumps every versioned file (`scripts/versioning/bump-versions.sh`, with a managed-paths allow-list), writes `CHANGELOG.md` **from closed issues** (`scripts/changelog/update_changelog_from_issues.sh`), commits to a staging ref `release-prepare/<run_id>`, builds every artifact through reusable workflows, verifies SHA-256s, promotes to `main` and tags, then dispatches `release.yml` with `prepare_run_id`. A plain tag push is only validated and cannot publish | the dispatch input; tags **without `v`** (`0.0.95`) | generated CHANGELOG plus `build-release-notes.ts` | many reusable build workflows (APK, IPA, jars, macOS helpers, desktop jpackage dmg/msi/deb) | per-artifact | SHA-256s recorded in a release registry and re-verified before publish; APK signature check; macOS notarization via notarytool with an API key | `npm publish --provenance`; a provenance JSON artifact that links the tag to the prepare run | Docker Hub via `docker/*` actions (amd64, continue-on-error); a Homebrew **formula** as its own terminal job (`update-brew-formula.sh`; `REQUIRE_TOKEN=1` turns a missing token into a loud failure); Conveyor desktop behind `vars.DESKTOP_PUBLISHER` | the release publishes **the exact prepared artifacts, never a rebuild** | `cache-mode: none` (no Actions caches in release builds); concurrency per tag; idempotent reruns (`gh release view` → upload `--clobber`) |
| **auto-mobile** `live-idle-release.yml` | cron, nightly, plus dispatch | n/a: an advisory nightly check of device-session "release" windows, not a software release | n/a | one emulator | n/a | n/a | n/a | n/a | it *is* the smoke test; uploads evidence | runs only if `github.repository == 'kaeawc/auto-mobile'` (fork guard); on failure it **opens or updates one tracking issue** |
| **android-build** `publish.yml` | `workflow_run` after "Commit" succeeds on `main` | content hashes (Artifact Swap) | n/a | one job | n/a | n/a | n/a | GitHub Packages (Maven); advances an `artifact-swap-green-main` branch forward only | n/a | **publishes only the exact `head_sha` that CI validated**; serialized concurrency; unpinned actions |
| **kaeawc/homebrew-tap** (GitHub; local `~/kaeawc/homebrew` is an empty directory) | n/a | n/a | n/a | n/a | n/a | n/a | n/a | `Formula/auto-mobile.rb` (npm tarball, `livecheck`), `Casks/krit.rb` (prebuilt darwin and linux, arm and intel). Release scripts clone with a PAT and push straight to `main` with the message `<name> <tag>`, no-op if unchanged. `formula-audit.yml` runs `brew style` / `audit --strict --online` / `install` / `test` on `Formula/**` but **hard-codes `auto-mobile`** | `brew test` in the tap | n/a |
| **rust-build today** | CI on push/PR; `desktop.yml` on path changes | `workspace.package.version = "0.1.0"`; `desktop/Cargo.toml` has its own literal `0.1.0` | none; no tags yet | CI `build` job: `cargo build --release -p rb` on ubuntu only, uploaded as `rb-linux-x86_64`; desktop builds on macOS only | n/a | n/a | n/a | n/a | n/a | everything SHA-pinned; `persist-credentials: false`; read-only token; **tags are immutable**; Actions allow-list is GitHub-owned plus `Swatinem/rust-cache` and `taiki-e/install-action`; Dependabot updates the pins |

### Conventions to keep

1. **`v`-prefixed SemVer tags pushed by hand** (spectra, krit, pyhotlint). A `-` suffix means a
   GitHub prerelease, and prereleases stop there: no Homebrew, no package managers, no `latest`
   (krit, spectra).
2. **A tag regex check up front that fails closed** (spectra, krit publish-maven-central,
   auto-mobile).
3. **`workflow_dispatch` is a dry run** that builds without signing or publishing (spectra).
4. **Matrix build, then a funnel job** that assembles `dist/`, writes one sorted checksum file
   covering every platform and creates the release with `gh release create` (krit).
5. **Hand-rolled shell in `scripts/release/*.sh`**, called from the workflow, so it can be run
   locally (all of them). Tools like goreleaser stay at the build step (krit even
   `--skip=publish`es it).
6. **`actions/attest-build-provenance`** on the release files (krit), and **SBOMs per archive**
   (krit via syft).
7. **Homebrew through `kaeawc/homebrew-tap`**, rendered from the checksum file, pushed with
   `HOMEBREW_TAP_TOKEN`, stable tags only, in a **separate terminal job** so a tap failure
   doesn't block the release (auto-mobile), with `REQUIRE_TOKEN=1` (auto-mobile).
8. **Secrets live in a protected `release` environment**, signing jobs restore no caches and
   don't persist credentials (spectra). Release builds restore no Actions caches at all
   (auto-mobile `cache-mode: none`).
9. **Publish exactly what was verified.** auto-mobile never rebuilds, and android-build publishes
   only the `head_sha` that CI validated.
10. **Smoke-test the shipped archive, not the build tree.** spectra extracts the native archive and
    checks the embedded version.
11. **Nightly with a skip-if-unchanged check** (krit), and **failures of an advisory nightly open
    or update one tracking issue** (auto-mobile live-idle).

### Conventions to avoid for this repo

- auto-mobile's prepare-release (staging refs, promote-and-tag, cross-workflow dispatch) solves
  problems rust-build doesn't have: many versioned files, many artifact kinds, PR-protected
  `main`. Here the owner pushes to `main` directly and has two version strings.
- krit's nightly tags (`v0.3.0-nightly.YYYYMMDD`) need a PAT to trigger the release and fill the
  Releases page. rust-build's tags are immutable, so a rolling `nightly` tag isn't possible
  either. Nightlies should be Actions artifacts instead (§2.8).
- GPG (krit) and the custom Ed25519 manifest (spectra) both need long-lived keys and a
  third-party action or custom tool. GitHub artifact attestations (keyless Sigstore, verified with
  `gh attestation verify`) cover the same need with no key to manage and no new action.
- `--generate-notes` (all of them) lists *merged PRs*. rust-build commits straight to `main`, so
  generated notes would be almost empty apart from Dependabot. Build the notes from `git log`
  between tags instead (§2.9).
- Unpinned actions and `softprops/action-gh-release` (pyhotlint), and `docker/*` actions
  (auto-mobile). rust-build's settings would reject them.

---

## 2. Proposed design

### 2.1 Constraints from the repo

- `rust-toolchain.toml` pins 1.99.1 (minimal profile). Per-target std is added with
  `rustup target add` in each job.
- `[profile.dist]`: thin LTO, one codegen unit, `strip = true`. That's what ships, and the
  Dockerfile already uses it.
- TLS runs on `ring`, with no OpenSSL or aws-lc, so the only C is ring's, and it builds with the
  system `cc` (or `musl-gcc`). This is what makes native builds and musl easy.
- `.cargo/config.toml` links Windows MSVC with `rust-lld`. CI never builds Windows today, so the
  first release run is also the first Windows build (see §4).
- `deny.toml [graph] targets` lists the 5 gnu/darwin/msvc triples. If the musl targets ship, add
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` there so `cargo deny` checks their
  dependency graph.
- `desktop/` is a separate workspace with its own `Cargo.lock`, a Rune git dependency pinned by
  `rev`, and `version = "0.1.0"` written out by hand (it can't inherit
  `workspace.package`). Its CI build skips `--locked` on purpose, because of Dependabot root
  bumps. The release build *uses* `--locked`, so a stale `desktop/Cargo.lock` fails the release
  loudly, and `bump.sh` refreshes it.
- On Linux, Rune needs `libxkbcommon-dev` and `libwayland-dev` at build time, and
  `libxkbcommon-x11-0` plus a Vulkan driver (`mesa-vulkan-drivers`, lavapipe) to open a window.
  A release Rune build only accepts `--snap` with the `rune/tooling` feature, but the
  **`RUNE_READY_DIR` ready receipt works in any build**, so the desktop smoke test uses that.
- Repository rules: tags are immutable and the workflow token is read-only by default, so each job
  elevates its own permissions. Every action must be pinned to a SHA.

### 2.2 Trigger and versioning

| Event | What happens |
| --- | --- |
| `push` of tag `v*` | Full pipeline. Publishing happens only if the tag matches `^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$`, equals `v` + `workspace.package.version`, equals the desktop version, the tagged commit is an ancestor of `origin/main`, and the **CI workflow concluded `success` on that exact SHA** (it waits up to 20 minutes, android-build's "only publish what CI validated"). |
| `workflow_dispatch` | Dry run on any ref: build, package, checksum and smoke everything. Docker uses `--load` and never pushes. Nothing is attested or published. Artifacts are kept for 14 days. |
| `schedule` (`0 6 * * *`) | Same dry run on `main`, **skipped if `main` hasn't moved since the last successful scheduled run**. This is the desktop (and CLI) nightly. On failure it opens or comments on one tracking issue. |

**Version bump** (local, owner): `scripts/release/bump.sh 0.2.0 [--push]` (§6.1). It checks
that the tree is clean, on `main` and level with `origin/main`, and that the tag doesn't exist
(tags are immutable). It then rewrites `workspace.package.version` and the desktop `[package]`
version, refreshes both lockfiles with `cargo metadata`, optionally turns `## [Unreleased]` in
`CHANGELOG.md` into `## [X.Y.Z] - date`, runs `cargo c && cargo lint && cargo t` and
`cd desktop && cargo check --locked`, commits `Release vX.Y.Z`, creates an annotated tag, and
with `--push` runs `git push --atomic origin main vX.Y.Z`. CI and the release start together, and
the release waits for CI.

Archive names use a **label**. On a tag, the label is the version (`0.2.0`). On a dry run it is
`0.2.0-dev.20261121.abc1234`, so nightly downloads are easy to tell apart. The binary always
reports the Cargo version.

### 2.3 Jobs

```
verify ──┬─ build-cli (7) ──────┐
         ├─ build-desktop (3) ──┴─ checksums ──┬─ smoke-cli (7) ─────┐
         │                                     └─ smoke-desktop (3) ─┤
         └─ docker (2: build, push-by-digest, smoke) ────────────────┴─ release ─┬─ attest-sbom (7)
                                                                                 ├─ ghcr (tag index + attest)
                                                                                 └─ homebrew (stable only)
  nightly-alert (schedule && failure)
```

`release` and everything after it run only when `publish == 'true'` (a validated tag push).
Channels that come after the GitHub release (`ghcr`, `homebrew`, `attest-sbom`) are separate
jobs that can be re-run on their own and are idempotent. That follows auto-mobile's lesson
that an optional channel shouldn't fail or skip the main release.

### 2.4 CLI targets and how each is built

| Target | Build runner | How | Archive | Smoke runner |
| --- | --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | native; glibc 2.35 baseline (older than 24.04's 2.39, so it runs on Debian 12, Ubuntu 22.04 and RHEL 10) | `.tar.gz` | `ubuntu-24.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | native arm64 runner (free for public repos) | `.tar.gz` | `ubuntu-24.04-arm` |
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` | native plus `musl-tools`, `CC_x86_64_unknown_linux_musl=musl-gcc` for ring; fully static | `.tar.gz` | `ubuntu-24.04` → `alpine` container |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | same, on arm64 | `.tar.gz` | `ubuntu-24.04-arm` → `alpine` container |
| `aarch64-apple-darwin` | `macos-15` | native; `MACOSX_DEPLOYMENT_TARGET=11.0`; the linker ad-hoc signs it | `.tar.gz` | `macos-15` |
| `x86_64-apple-darwin` | `macos-15` | cross from arm64 (`rustup target add`; Apple clang ships both SDKs, krit does the same) | `.tar.gz` | `macos-15-intel` (actual Intel) |
| `x86_64-pc-windows-msvc` | `windows-2025` | native, `rust-lld` (repo config), `-C target-feature=+crt-static` so there's no VC++ redistributable dependency | `.zip` | `windows-2025` |

Why gnu *and* musl: gnu is the default (Homebrew on Linux, fastest allocator for a load
generator), and musl is the static, portable one (Alpine, ancient distros, scratch containers),
matching krit's extra `linux_musl` archive. Dropping musl leaves 5 targets if the matrix needs
trimming. musl's malloc is noticeably slower under tokio's multi-threaded load, and the release
notes should say so.

Not chosen:

- **`cross`:** Docker plus QEMU for tests. It isn't needed now that native arm64 runners exist.
- **cargo-zigbuild:** only worth it to target a glibc older than 22.04's
  (`--target x86_64-unknown-linux-gnu.2.28`). It would need `cargo-zigbuild` (taiki-e can
  install it) and zig itself (pip or curl). Keep it in reserve.
- **cargo-dist:** it generates and owns its own `release.yml` (checked by `dist plan`), installs
  itself via `curl | sh`, and handles neither the separate `desktop/` workspace nor the
  distroless Dockerfile. The owner's style is hand-rolled shell plus `gh`.

Archive layout follows the ripgrep and cargo-binstall convention, and the owner's
"one top-level directory" convention from spectra and pyhotlint:

```
rb-0.2.0-aarch64-apple-darwin.tar.gz
└── rb-0.2.0-aarch64-apple-darwin/
    ├── rb            (rb.exe on Windows)
    ├── README.md
    └── LICENSE
rb-0.2.0-aarch64-apple-darwin.cdx.json   (CycloneDX SBOM for that target)
```

The `<name>-<version>-<target>` pattern is one that cargo-binstall's default URL templates try,
so `cargo binstall --git https://github.com/kaeawc/rust-build rb` can find the archives. This is
untested, because `publish = false` keeps rb off crates.io.

### 2.5 Checksums and attestations

- **`SHA256SUMS`**: `sha256sum` format, sorted by file name, covering every CLI archive, SBOM and
  desktop archive. It's written once in the `checksums` job (krit's funnel). Users verify with
  `sha256sum -c --ignore-missing SHA256SUMS`. krit and spectra call this file `checksums.txt`.
  Pick one name across the owner's tools; this doc uses `SHA256SUMS` as requested.
- **Build provenance**: `actions/attest` with `subject-checksums: dist/SHA256SUMS` makes one SLSA
  provenance attestation covering every file. It runs in the `release` job *before* the release
  is published, then checks itself with `gh attestation verify`. Users run
  `gh attestation verify rb-0.2.0-x86_64-unknown-linux-gnu.tar.gz -R kaeawc/rust-build`.
- **SBOM**: `cargo cyclonedx --target <triple>` (installed with `taiki-e/install-action`, which has
  a `cargo-cyclonedx` manifest) runs in each build job, so the SBOM lists exactly that target's
  dependency graph. The SBOMs ship as release assets and get an `actions/attest` SBOM
  attestation per archive (`sbom-path`), in a small matrix job after the release. The repo's
  GitHub dependency-graph SBOM (`gh api repos/kaeawc/rust-build/dependency-graph/sbom`) works as
  a fallback that needs no install, but it covers the whole repo, including dev-deps and
  desktop.
- `actions/attest-build-provenance` and `actions/attest-sbom` are now thin wrappers, and
  attest-sbom prints a deprecation warning. Use **`actions/attest`** (v4.2.2) directly. It needs
  `id-token: write`, `attestations: write` and `artifact-metadata: write`. Storage records are
  org-only, so use `create-storage-record: false` for the image (the repo is user-owned).
- Not done: GPG, minisign or a custom manifest signature. If an offline-verifiable signature is
  wanted later, spectra's pattern applies: a `sign` job in the `release` environment, no caches,
  a key from an environment secret, and a detached signature of `SHA256SUMS`.

### 2.6 GHCR image

- Image: `ghcr.io/kaeawc/rust-build`. Tags: `X.Y.Z` always; `X.Y` and `latest` for stable tags
  only.
- **Native per-arch builds** on `ubuntu-24.04` (amd64) and `ubuntu-24.04-arm` (arm64) from the
  unchanged `ci/Dockerfile` (`rust:1.99.1-slim-bookworm` → `distroless/cc-debian12:nonroot`).
  No QEMU, because thin-LTO Rust under emulation is very slow.
- No `docker/*` actions. The steps are `docker buildx create --driver docker-container`,
  `docker login ghcr.io` with `GITHUB_TOKEN` on stdin, and
  `docker buildx build --provenance=mode=max --sbom=true --output type=image,push-by-digest=true,...`,
  which gives BuildKit SLSA provenance and an SPDX SBOM as OCI attestation manifests.
  `SOURCE_DATE_EPOCH` is the commit time, with `rewrite-timestamp=true`, for reproducible layers.
  Each arch job **pulls its own digest and smoke-tests it** (`--version`, `/healthz`, `/readyz`,
  non-root user) before anything gets a tag.
- The `ghcr` job (after `release`) runs `docker buildx imagetools create` with
  `index:org.opencontainers.image.*` annotations. GHCR reads `source` to link the package to the
  repo and shows `description`. It then attests the index digest with
  `actions/attest` (`push-to-registry: true`) and verifies with
  `gh attestation verify oci://ghcr.io/kaeawc/rust-build:X.Y.Z -R kaeawc/rust-build`.
- The image binary is built from source by the Dockerfile rather than copied from the release
  archive. That keeps the glibc match between builder and runtime (bookworm → debian12) and
  needs no Dockerfile change. Both come from the same tagged tree with `--locked`.
- One-time step: the first push creates the package as **private**. Set it to public in the
  package settings, and confirm "Inherit access from source repository".

### 2.7 Homebrew

- **Formula** `Formula/rust-build.rb` in `kaeawc/homebrew-tap`, installing `bin/rb` from the
  prebuilt archives: on_macos arm/intel, and on_linux arm/intel using the **gnu** builds. It
  includes `livecheck :github_latest` and `test do assert_match "rb #{version}", ...`. A formula
  rather than krit's cask, because it's a CLI, the tap's `formula-audit.yml` already runs
  `brew audit`/`install`/`test` on `Formula/**`, and casks are for apps. The desktop app could
  be `Casks/rust-build-desktop.rb` later, but only once it's notarized.
- Rendered by `scripts/release/update-brew-formula.sh` (§6.5) from `SHA256SUMS`. It clones the
  tap with `HOMEBREW_TAP_TOKEN` (a fine-grained PAT with Contents: write on `homebrew-tap`, stored
  in the `release` environment), commits `rust-build vX.Y.Z` and pushes. It does nothing if
  unchanged, and `REQUIRE_TOKEN=1` fails loudly when the token is missing. This is krit's script
  with auto-mobile's guard.
- Stable tags only. It can be turned off with the repo variable `HOMEBREW_TAP=false`.
- Tap follow-up (in the tap repo): `formula-audit.yml` hard-codes `auto-mobile`. Make it iterate
  over the changed `Formula/*.rb`, or add `rust-build`. That workflow also uses
  `Homebrew/actions/setup-homebrew@master` unpinned, which the tap's settings allow but
  rust-build's wouldn't.
- Users install with `brew install kaeawc/tap/rust-build`, which puts `rb` on PATH.

### 2.8 Desktop app: release and nightly

| | Release (tag) | Nightly / dispatch |
| --- | --- | --- |
| Targets | `aarch64-apple-darwin` (`macos-15`), `x86_64-unknown-linux-gnu` (`ubuntu-22.04`), `x86_64-pc-windows-msvc` (`windows-2025`) | same |
| Build | `cd desktop && cargo build --locked --release --target <t>`, with `CARGO_NET_GIT_FETCH_WITH_CLI=true` and no cache | same |
| macOS | `rust-build.app` (minimal `Info.plist`, bundle id `com.kaeawc.rust-build.desktop`) **ad-hoc signed**, zipped with `ditto` → `rb-desktop-<label>-aarch64-apple-darwin.zip` | same |
| Linux | `rb-desktop-<label>-x86_64-unknown-linux-gnu.tar.gz`. The README notes the runtime deps: `libxkbcommon-x11-0`, `libwayland-client0`, a Vulkan driver | same |
| Windows | `rb-desktop-<label>-x86_64-pc-windows-msvc.zip` | same |
| Where | GitHub release assets, in `SHA256SUMS`, with provenance attestation | Actions artifacts `desktop-<target>`, 14-day retention |
| Gate | blocks the release, all or nothing. The repo variable `RELEASE_DESKTOP=false` drops the desktop jobs, and the release still runs | failure opens a tracking issue |

Desktop shares the CLI version and tag for now. If it ever needs its own cadence, use krit's
separate tag namespace (`desktop-vX.Y.Z`) with a separate workflow.

**Signing, phase 2** (not in the draft YAML): an unsigned or ad-hoc signed `.app` gets
quarantined by Gatekeeper, so users need `xattr -dr com.apple.quarantine rust-build.app`. To
fix that, add a `sign-macos` job in the `release` environment between `build-*` and `checksums`.
It would import a Developer ID `.p12` into a temporary keychain, run
`codesign --options runtime --timestamp` on `rb` (both darwin archives) and the `.app`, notarize
with `xcrun notarytool submit --key <ASC API key> --wait`, `stapler staple` the `.app`, then
repackage and re-upload with `overwrite: true`. This is spectra's `CODESIGN_ID` and
`notarize-archives.sh`, or auto-mobile's notarytool flow. Gate it with
`vars.MACOS_SIGNING == 'true'` and give `checksums` the same `!cancelled() && !failure()`
condition. It needs secrets but no new actions. Windows Authenticode is out of scope.

Small desktop fixes before the first release:

- Add `#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]` to
  `desktop/src/main.rs`, so a console window doesn't open with the app.
- Optionally add a `[profile.dist]` to `desktop/Cargo.toml`.

### 2.9 Release notes

`scripts/release/notes.sh` (§6.4) writes:

1. the `## [X.Y.Z]` section of `CHANGELOG.md`, if the file exists. Adopting krit's Keep a
   Changelog file is recommended but optional.
2. `git log --no-merges --format='- %s (%h)' <prev-tag>..<tag>`. With direct pushes to `main`,
   this is the real change list.
3. Install snippets (brew, docker, curl) and verify snippets (`sha256sum -c`,
   `gh attestation verify` for a file and for the image), plus the Gatekeeper note for the
   desktop app.

### 2.10 Smoke tests

| Artifact | Runner | Checks |
| --- | --- | --- |
| each CLI archive | its own OS and arch, on a clean runner that downloads only `release-dist` | its `SHA256SUMS` line verifies; it extracts; `rb --version` prints `rb <Cargo version>`; `rb serve` on a random port; `rb get /healthz` and `rb get -i /api/info`; `rb loadgen -c 4 -d 2s`; `rb serve --self-signed` plus `rb get -k -i` shows `HTTP/2.0` (ALPN); `rb cert --out` writes `cert.pem` |
| musl archives | same, **inside `alpine`** | the same script under Alpine's busybox userland, which proves the binary is static |
| `x86_64-apple-darwin` | `macos-15-intel` | real Intel, not Rosetta |
| image (per arch) | the arch's own runner, pulled by digest | `--entrypoint /rb … --version`; `docker run -p` then `curl /healthz` and `/readyz`; `Config.User` is non-root |
| desktop (per OS) | its OS; Linux under `xvfb-run` with lavapipe | its checksum line verifies; it extracts; it launches with `RUNE_READY_DIR` and must write its ready receipt (`{"pid":…,"ready_ms":…}`) within 60 s. Windows starts with `continue-on-error` until it has been seen to pass on hosted runners |

### 2.11 Hardening

- Workflow default `permissions: contents: read`. Each job asks only for what it needs:
  `release` gets `contents: write` and the attest permissions, `docker` gets `packages: write`,
  `ghcr` gets `packages: write` and the attest permissions, `nightly-alert` gets `issues: write`.
  Build jobs never get `id-token: write`, so dependency build scripts can't mint OIDC tokens.
- `persist-credentials: false` on every checkout (the repo convention).
- No `Swatinem/rust-cache` and no other Actions cache in this workflow (spectra, auto-mobile).
  Every release builds from scratch with `--locked`. The cost is a few extra minutes per
  target.
- `concurrency: release-${{ github.ref }}` without `cancel-in-progress`.
- Jobs that hold secrets or publish (`release`, `ghcr`, `homebrew`) run in a protected
  **`release` environment**. Restrict its deployments to tags `v*`, and optionally add the owner
  as a required reviewer for a final click before anything goes public.
- The release is created as a **draft**, all assets are uploaded and attested, and only then is
  it published. That works with GitHub's immutable releases (published assets can't change) and
  makes reruns idempotent: an existing draft gets `upload --clobber`, and an already-published
  release is a no-op.
- Dependabot's `github-actions` ecosystem already covers new workflow files, so the pins below
  stay current.

---

## 3. Actions required

Every action below is GitHub-owned or already on the allow-list. **No new third-party action is
needed.**

| Action | Pin | Status |
| --- | --- | --- |
| `actions/checkout` | `3d3c42e5aac5ba805825da76410c181273ba90b1` (v7.0.1) | already used in rust-build |
| `actions/upload-artifact` | `cf430e030ddbb5b0abf93d22962f4752f3646cd9` (v7.0.2) | already used in rust-build |
| `actions/download-artifact` | `9000827ccba6bdab643e8b6fd33ac0654aef8333` (v8.0.2) | new to this repo; GitHub-owned. Same pin as spectra; latest release, SHA checked against the tag |
| `actions/attest` | `1e69f48acb82d1966a394da916b4c1698aa569d6` (v4.2.2) | new to this repo; GitHub-owned. Latest release, SHA checked. Replaces `attest-build-provenance` (krit pins `4d101475d8b20a2381f78447822ac1eab6504dd8`, v4.2.2, now a wrapper) and `attest-sbom` (deprecated; v4.1.0 is `c604332985a26aa8cf1bdc465b92731239ec6b9e`) |
| `taiki-e/install-action` | `25f51f6b1bf4e18876107cece836f4f77ac17c8c` (v2.90.7) | already allowed and used; installs `cargo-cyclonedx` (manifest present at this SHA) |

Images used by `docker run` in smoke steps aren't actions, but they should be digest-pinned:
`alpine:3.22@sha256:TODO`. The Dockerfile's base images are already a separate question,
`rust:1.99.1-slim-bookworm` and `distroless/cc-debian12:nonroot`; pin them by digest and let
Dependabot's `docker` ecosystem bump them.

If plain CLI ever isn't enough, these *would* need adding to the allow-list, with TODO SHAs:
`docker/setup-buildx-action`, `docker/login-action`, `docker/metadata-action`,
`docker/build-push-action`, `docker/setup-qemu-action` (auto-mobile's set);
`anchore/sbom-action` (krit; not needed, since taiki-e can install `syft`);
`crazy-max/ghaction-import-gpg` (krit, only for GPG); `goreleaser/goreleaser-action` (Go only);
`softprops/action-gh-release` (pyhotlint; `gh` replaces it).

---

## 4. Rollout checklist and open questions

Before the first tag:

1. Add `scripts/release/{bump,package-cli,package-desktop,smoke-cli,smoke-desktop,notes,update-brew-formula}.sh`
   (§6) and `.github/workflows/release.yml` (§5). Optionally add `make release VERSION=x.y.z` →
   `scripts/release/bump.sh`.
2. Add the musl triples to `deny.toml [graph] targets` (or drop musl).
3. Create the **`release` environment**: deployment rule tags `v*`, secret `HOMEBREW_TAP_TOKEN`.
   Optional repo variables: `RELEASE_DESKTOP`, `HOMEBREW_TAP`.
4. Run the workflow once with **`workflow_dispatch`**. It is the first Windows and macOS build of
   the root workspace (CI is Linux-only), the first Linux and Windows desktop build, and the
   first run of `cargo cyclonedx`. Things to watch:
   - **Windows `rust-lld` plus `+crt-static`.** Fallback:
     `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=link.exe` for the release only.
   - **`cargo cyclonedx` output path and name** with `--override-filename`. The draft expects
     `apps/rb/sbom.json`; adjust after the first run.
   - **Runner labels.** `ubuntu-22.04-arm` and `macos-15-intel` need to exist and be
     public-repo-free. `macos-15-intel` is the last Intel image; when it goes away, smoke
     x86_64-darwin under Rosetta on `macos-15`, or drop it. Docker also needs to be present on
     the arm64 Ubuntu image.
   - **Desktop on Linux 22.04**, with its libxkbcommon and wayland dev packages. Fall back to
     `ubuntu-24.04` (glibc 2.39 baseline) if Rune needs newer ones.
   - **The desktop ready receipt under Xvfb and on Windows hosted runners.** The `RUNE_READY_DIR`
     path translation in Git Bash is handled with `cygpath -w`.
5. Fix the desktop console window on Windows (`windows_subsystem`), and decide whether the app
   ships with `rune/tooling` (adds `--snap` to release builds, about 3.5 MB).
6. After the first publish: make the GHCR package public, and update the tap's
   `formula-audit.yml`.

Open questions:

- Name the checksum file `SHA256SUMS` (requested here) or `checksums.txt` (krit and spectra
  consistency)?
- Ship musl at all, given its allocator slowdown for `loadgen` and `serve`? Making the musl
  build use a faster allocator would mean a new dependency, so it would go through
  `[workspace.dependencies]` and `deny`.
- Should the Docker image ship the attested release binary instead of rebuilding it? That would
  need a second Dockerfile stage that copies `rb` from the build context, and the gnu build's
  glibc would have to stay at or below Debian 12's (2.36). The 22.04 build (2.35) meets that.
- Required reviewer on the `release` environment: a small cost, and a last check that the
  immutable tag points at the right commit before anything goes public.

---

## 5. Draft `.github/workflows/release.yml`

```yaml
name: Release

# Tag push (vX.Y.Z[-pre]) builds, smoke-tests and publishes: GitHub release + attestations,
# GHCR multi-arch image, Homebrew formula (stable only). workflow_dispatch and the nightly
# schedule run the same build + smoke pipeline as a dry run: nothing is pushed or published, and
# the artifacts (desktop nightlies included) are kept for 14 days.
on:
  push:
    tags: ["v*"]
  workflow_dispatch:
  schedule:
    # 06:00 UTC daily; skipped when main hasn't moved since the last successful nightly.
    - cron: "0 6 * * *"

concurrency:
  group: release-${{ github.ref }}
  cancel-in-progress: false

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0
  CARGO_NET_RETRY: 10
  RUSTUP_MAX_RETRIES: 10
  RUST_BACKTRACE: short
  IMAGE: ghcr.io/kaeawc/rust-build

# No Actions caches anywhere in this workflow: release builds start from scratch with --locked.

jobs:
  verify:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    permissions:
      contents: read
      actions: read # gh run list: CI status of the tagged commit, last nightly
    outputs:
      version: ${{ steps.v.outputs.version }}
      label: ${{ steps.v.outputs.label }}
      tag: ${{ steps.v.outputs.tag }}
      sha: ${{ steps.v.outputs.sha }}
      epoch: ${{ steps.v.outputs.epoch }}
      publish: ${{ steps.v.outputs.publish }}
      prerelease: ${{ steps.v.outputs.prerelease }}
      build: ${{ steps.changed.outputs.build }}
      retention: ${{ steps.v.outputs.retention }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Resolve and validate version
        id: v
        env:
          EVENT: ${{ github.event_name }}
          REF_TYPE: ${{ github.ref_type }}
          REF_NAME: ${{ github.ref_name }}
        run: |
          set -euo pipefail
          version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
          desktop=$(python3 -c 'import tomllib; print(tomllib.load(open("desktop/Cargo.toml","rb"))["package"]["version"])')
          if [[ "$desktop" != "$version" ]]; then
            echo "::error::desktop/Cargo.toml version $desktop != workspace.package.version $version (run scripts/release/bump.sh)"
            exit 1
          fi
          sha=$(git rev-parse HEAD)
          if [[ "$EVENT" == "push" && "$REF_TYPE" == "tag" ]]; then
            tag="$REF_NAME"
            if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
              echo "::error::tag must be vX.Y.Z or vX.Y.Z-<pre>, got $tag"; exit 1
            fi
            if [[ "${tag#v}" != "$version" ]]; then
              echo "::error::tag $tag does not match workspace.package.version $version"; exit 1
            fi
            publish=true; label="$version"; retention=7
          else
            tag="v$version"; publish=false; retention=14
            label="${version}-dev.$(date -u +%Y%m%d).${sha:0:7}"
          fi
          prerelease=false
          [[ "$version" == *-* ]] && prerelease=true
          {
            echo "version=$version"
            echo "label=$label"
            echo "tag=$tag"
            echo "sha=$sha"
            echo "epoch=$(git log -1 --format=%ct)"
            echo "publish=$publish"
            echo "prerelease=$prerelease"
            echo "retention=$retention"
          } >> "$GITHUB_OUTPUT"
      - name: Skip the nightly if main hasn't moved
        id: changed
        env:
          GH_TOKEN: ${{ github.token }}
          EVENT: ${{ github.event_name }}
          SHA: ${{ steps.v.outputs.sha }}
        run: |
          set -euo pipefail
          build=true
          if [[ "$EVENT" == "schedule" ]]; then
            last=$(gh run list -R "$GITHUB_REPOSITORY" --workflow release.yml --event schedule \
              --status success --limit 1 --json headSha --jq '.[0].headSha // ""')
            if [[ "$last" == "$SHA" ]]; then
              echo "No commits since the last successful nightly ($SHA); skipping."
              build=false
            fi
          fi
          echo "build=$build" >> "$GITHUB_OUTPUT"
      - name: Tagged commit is on main
        if: steps.v.outputs.publish == 'true'
        run: |
          set -euo pipefail
          git fetch --no-tags origin main
          git merge-base --is-ancestor HEAD FETCH_HEAD || {
            echo "::error::$(git rev-parse HEAD) is not on main"; exit 1; }
      - name: CI passed on the tagged commit
        if: steps.v.outputs.publish == 'true'
        env:
          GH_TOKEN: ${{ github.token }}
          SHA: ${{ steps.v.outputs.sha }}
        run: |
          set -euo pipefail
          # The tag usually lands with its commit (git push --atomic origin main vX.Y.Z), so CI may
          # still be running: wait up to 20 minutes for it.
          for _ in $(seq 1 60); do
            result=$(gh run list -R "$GITHUB_REPOSITORY" --workflow ci.yml --commit "$SHA" \
              --json status,conclusion \
              --jq '[.[] | select(.status == "completed")] | .[0].conclusion // "pending"')
            case "$result" in
              success) echo "CI green on $SHA"; exit 0 ;;
              pending) sleep 20 ;;
              *) echo "::error::CI on $SHA concluded '$result'; re-run CI for that commit, then this job"; exit 1 ;;
            esac
          done
          echo "::error::CI did not finish on $SHA within 20 minutes"; exit 1

  build-cli:
    name: build rb (${{ matrix.target }})
    needs: verify
    if: needs.verify.outputs.build == 'true'
    runs-on: ${{ matrix.os }}
    timeout-minutes: 45
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: x86_64-unknown-linux-gnu, os: ubuntu-22.04 }
          - { target: aarch64-unknown-linux-gnu, os: ubuntu-22.04-arm }
          - { target: x86_64-unknown-linux-musl, os: ubuntu-24.04, musl: true }
          - { target: aarch64-unknown-linux-musl, os: ubuntu-24.04-arm, musl: true }
          - { target: aarch64-apple-darwin, os: macos-15 }
          - { target: x86_64-apple-darwin, os: macos-15 }
          - { target: x86_64-pc-windows-msvc, os: windows-2025 }
    defaults:
      run:
        shell: bash
    env:
      TARGET: ${{ matrix.target }}
      LABEL: ${{ needs.verify.outputs.label }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: rustup show active-toolchain || rustup toolchain install
      - run: rustup target add "$TARGET"
      - name: Install musl-tools
        if: matrix.musl
        run: sudo apt-get update -q && sudo apt-get install -y -q --no-install-recommends musl-tools
      - name: Build
        env:
          # ring's C/asm for musl targets.
          CC_x86_64_unknown_linux_musl: musl-gcc
          CC_aarch64_unknown_linux_musl: musl-gcc
          # Static CRT: no VC++ redistributable needed on the user's machine.
          CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS: -C target-feature=+crt-static
          MACOSX_DEPLOYMENT_TARGET: "11.0"
        run: cargo build --locked --profile dist -p rb --target "$TARGET"
      - uses: taiki-e/install-action@25f51f6b1bf4e18876107cece836f4f77ac17c8c # v2.90.7
        with:
          tool: cargo-cyclonedx
      - name: SBOM (CycloneDX, this target's dependency graph)
        run: |
          set -euo pipefail
          cargo cyclonedx --format json --target "$TARGET" --override-filename sbom -q
          mkdir -p dist
          # TODO(first run): confirm cargo-cyclonedx 0.5's output path/extension.
          cp apps/rb/sbom.json "dist/rb-${LABEL}-${TARGET}.cdx.json"
      - name: Package
        run: bash scripts/release/package-cli.sh "$LABEL" "$TARGET"
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        with:
          name: cli-${{ matrix.target }}
          path: dist/*
          if-no-files-found: error
          retention-days: ${{ fromJSON(needs.verify.outputs.retention) }}

  build-desktop:
    name: build desktop (${{ matrix.target }})
    needs: verify
    if: needs.verify.outputs.build == 'true' && vars.RELEASE_DESKTOP != 'false'
    runs-on: ${{ matrix.os }}
    timeout-minutes: 60
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: aarch64-apple-darwin, os: macos-15 }
          - { target: x86_64-unknown-linux-gnu, os: ubuntu-22.04 }
          - { target: x86_64-pc-windows-msvc, os: windows-2025 }
    defaults:
      run:
        shell: bash
    env:
      TARGET: ${{ matrix.target }}
      LABEL: ${{ needs.verify.outputs.label }}
      VERSION: ${{ needs.verify.outputs.version }}
      CARGO_NET_GIT_FETCH_WITH_CLI: "true"
      MACOSX_DEPLOYMENT_TARGET: "11.0"
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: rustup show active-toolchain || rustup toolchain install
      - run: rustup target add "$TARGET"
      - name: Install Rune's Linux build deps
        if: runner.os == 'Linux'
        run: sudo apt-get update -q && sudo apt-get install -y -q --no-install-recommends libxkbcommon-dev libwayland-dev
      # --locked here (unlike desktop.yml): a stale desktop/Cargo.lock must fail the release.
      - name: Build
        working-directory: desktop
        run: cargo build --locked --release --target "$TARGET"
      - name: Package
        run: bash scripts/release/package-desktop.sh "$LABEL" "$VERSION" "$TARGET"
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        with:
          name: desktop-${{ matrix.target }}
          path: dist/*
          if-no-files-found: error
          retention-days: ${{ fromJSON(needs.verify.outputs.retention) }}

  # Funnel: one sorted SHA256SUMS over every file that will be released.
  checksums:
    needs: [verify, build-cli, build-desktop]
    # build-desktop may be skipped (RELEASE_DESKTOP=false); a skipped need must not skip this.
    if: ${{ !cancelled() && !failure() && needs.verify.outputs.build == 'true' }}
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          pattern: cli-*
          path: dist
          merge-multiple: true
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          pattern: desktop-*
          path: dist
          merge-multiple: true
      - name: SHA256SUMS
        run: |
          set -euo pipefail
          cd dist
          sha256sum -- * | sort -k2 > "$RUNNER_TEMP/SHA256SUMS"
          mv "$RUNNER_TEMP/SHA256SUMS" SHA256SUMS
          cat SHA256SUMS
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        with:
          name: release-dist
          path: dist/
          if-no-files-found: error
          retention-days: ${{ fromJSON(needs.verify.outputs.retention) }}

  smoke-cli:
    name: smoke rb (${{ matrix.target }})
    needs: [verify, checksums]
    runs-on: ${{ matrix.os }}
    timeout-minutes: 10
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: x86_64-unknown-linux-gnu, os: ubuntu-24.04 }
          - { target: aarch64-unknown-linux-gnu, os: ubuntu-24.04-arm }
          - { target: x86_64-unknown-linux-musl, os: ubuntu-24.04, alpine: true }
          - { target: aarch64-unknown-linux-musl, os: ubuntu-24.04-arm, alpine: true }
          - { target: aarch64-apple-darwin, os: macos-15 }
          - { target: x86_64-apple-darwin, os: macos-15-intel }
          - { target: x86_64-pc-windows-msvc, os: windows-2025 }
    defaults:
      run:
        shell: bash
    env:
      TARGET: ${{ matrix.target }}
      LABEL: ${{ needs.verify.outputs.label }}
      VERSION: ${{ needs.verify.outputs.version }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          sparse-checkout: scripts/release
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: release-dist
          path: dist
      - name: Smoke (host)
        if: ${{ !matrix.alpine }}
        run: bash scripts/release/smoke-cli.sh dist "$LABEL" "$VERSION" "$TARGET"
      - name: Smoke (Alpine, proves the musl binary is static)
        if: matrix.alpine
        # TODO: pin alpine:3.22@sha256:<digest>.
        run: |
          docker run --rm -v "$PWD:/w" -w /w -e LABEL -e VERSION -e TARGET alpine:3.22 \
            sh -c 'apk add --no-cache bash >/dev/null && bash scripts/release/smoke-cli.sh dist "$LABEL" "$VERSION" "$TARGET"'

  smoke-desktop:
    name: smoke desktop (${{ matrix.target }})
    needs: [verify, checksums]
    if: vars.RELEASE_DESKTOP != 'false'
    runs-on: ${{ matrix.os }}
    timeout-minutes: 10
    # Windows hosted-runner GPU/WARP launch isn't proven yet; flip to false once it is.
    continue-on-error: ${{ matrix.experimental }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: aarch64-apple-darwin, os: macos-15, experimental: false }
          - { target: x86_64-unknown-linux-gnu, os: ubuntu-24.04, experimental: false }
          - { target: x86_64-pc-windows-msvc, os: windows-2025, experimental: true }
    defaults:
      run:
        shell: bash
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          sparse-checkout: scripts/release
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: release-dist
          path: dist
      - name: Install a display and software Vulkan
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update -q
          sudo apt-get install -y -q --no-install-recommends xvfb libxkbcommon-x11-0 libwayland-client0 mesa-vulkan-drivers
      - name: Launch and wait for Rune's ready receipt
        env:
          TARGET: ${{ matrix.target }}
          LABEL: ${{ needs.verify.outputs.label }}
        run: bash scripts/release/smoke-desktop.sh dist "$LABEL" "$TARGET"

  # Per-arch image: native build (no QEMU), push by digest only when publishing, smoke the digest.
  docker:
    name: image (${{ matrix.arch }})
    needs: verify
    if: needs.verify.outputs.build == 'true'
    runs-on: ${{ matrix.os }}
    timeout-minutes: 45
    permissions:
      contents: read
      packages: write
    strategy:
      fail-fast: false
      matrix:
        include:
          - { arch: amd64, platform: linux/amd64, os: ubuntu-24.04 }
          - { arch: arm64, platform: linux/arm64, os: ubuntu-24.04-arm }
    env:
      PUBLISH: ${{ needs.verify.outputs.publish }}
      VERSION: ${{ needs.verify.outputs.version }}
      SHA: ${{ needs.verify.outputs.sha }}
      SOURCE_DATE_EPOCH: ${{ needs.verify.outputs.epoch }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      # docker-container driver: needed for attestations and push-by-digest.
      - name: Set up buildx
        run: docker buildx create --name rb --driver docker-container --bootstrap --use
      - name: Log in to GHCR
        if: env.PUBLISH == 'true'
        env:
          GH_TOKEN: ${{ github.token }}
        run: echo "$GH_TOKEN" | docker login ghcr.io -u "$GITHUB_ACTOR" --password-stdin
      - name: Build
        id: build
        env:
          PLATFORM: ${{ matrix.platform }}
        run: |
          set -euo pipefail
          args=(--file ci/Dockerfile --platform "$PLATFORM"
            --label "org.opencontainers.image.source=https://github.com/${GITHUB_REPOSITORY}"
            --label "org.opencontainers.image.revision=${SHA}"
            --label "org.opencontainers.image.version=${VERSION}"
            --label "org.opencontainers.image.licenses=MIT")
          if [[ "$PUBLISH" == "true" ]]; then
            args+=(--provenance=mode=max --sbom=true --metadata-file "$RUNNER_TEMP/meta.json"
              --output "type=image,name=${IMAGE},push-by-digest=true,name-canonical=true,push=true,rewrite-timestamp=true")
            docker buildx build "${args[@]}" .
            digest=$(jq -r '."containerimage.digest"' "$RUNNER_TEMP/meta.json")
            ref="${IMAGE}@${digest}"
            docker pull "$ref"
          else
            docker buildx build "${args[@]}" --load --tag rb:smoke .
            digest=""; ref="rb:smoke"
          fi
          echo "digest=$digest" >> "$GITHUB_OUTPUT"
          echo "ref=$ref" >> "$GITHUB_OUTPUT"
      - name: Smoke
        env:
          REF: ${{ steps.build.outputs.ref }}
        run: |
          set -euo pipefail
          got=$(docker run --rm --entrypoint /rb "$REF" --version)
          [[ "$got" == "rb $VERSION" ]] || { echo "::error::image reports '$got'"; exit 1; }
          user=$(docker image inspect "$REF" --format '{{.Config.User}}')
          [[ -n "$user" && "$user" != "root" && "$user" != "0" ]] || { echo "::error::image runs as root"; exit 1; }
          cid=$(docker run -d -p 127.0.0.1:18080:8080 "$REF")
          trap 'docker logs "$cid" || true; docker rm -f "$cid" >/dev/null' EXIT
          for _ in $(seq 1 50); do curl -fsS http://127.0.0.1:18080/healthz >/dev/null 2>&1 && break; sleep 0.2; done
          curl -fsS http://127.0.0.1:18080/healthz
          curl -fsS http://127.0.0.1:18080/readyz
      - name: Record digest
        if: env.PUBLISH == 'true'
        env:
          DIGEST: ${{ steps.build.outputs.digest }}
        run: mkdir -p "$RUNNER_TEMP/digests" && echo "$DIGEST" > "$RUNNER_TEMP/digests/${{ matrix.arch }}"
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        if: env.PUBLISH == 'true'
        with:
          name: digest-${{ matrix.arch }}
          path: ${{ runner.temp }}/digests/*
          if-no-files-found: error
          retention-days: 1

  release:
    needs: [verify, checksums, smoke-cli, smoke-desktop, docker]
    if: ${{ !cancelled() && !failure() && needs.verify.outputs.publish == 'true' }}
    runs-on: ubuntu-latest
    timeout-minutes: 15
    environment: release
    permissions:
      contents: write
      id-token: write
      attestations: write
      artifact-metadata: write
    env:
      GH_TOKEN: ${{ github.token }}
      GH_REPO: ${{ github.repository }}
      TAG: ${{ needs.verify.outputs.tag }}
      VERSION: ${{ needs.verify.outputs.version }}
      PRERELEASE: ${{ needs.verify.outputs.prerelease }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: release-dist
          path: dist
      - name: Build provenance for every released file
        uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2
        with:
          subject-checksums: dist/SHA256SUMS
      - name: Release notes
        run: bash scripts/release/notes.sh "$TAG" > "$RUNNER_TEMP/notes.md"
      - name: Draft, upload, verify, publish
        run: |
          set -euo pipefail
          if is_draft=$(gh release view "$TAG" --json isDraft --jq .isDraft 2>/dev/null); then
            if [[ "$is_draft" == "false" ]]; then echo "$TAG is already published"; exit 0; fi
            gh release upload "$TAG" dist/* --clobber
          else
            args=(--draft --verify-tag --title "rust-build $TAG" --notes-file "$RUNNER_TEMP/notes.md")
            [[ "$PRERELEASE" == "true" ]] && args+=(--prerelease)
            gh release create "$TAG" "${args[@]}" dist/*
          fi
          gh attestation verify "dist/rb-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" -R "$GH_REPO"
          latest=--latest
          [[ "$PRERELEASE" == "true" ]] && latest=--latest=false
          gh release edit "$TAG" --draft=false "$latest"

  attest-sbom:
    name: SBOM attestation (${{ matrix.target }})
    needs: [verify, release]
    if: needs.release.result == 'success'
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
      id-token: write
      attestations: write
      artifact-metadata: write
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: x86_64-unknown-linux-gnu, ext: tar.gz }
          - { target: aarch64-unknown-linux-gnu, ext: tar.gz }
          - { target: x86_64-unknown-linux-musl, ext: tar.gz }
          - { target: aarch64-unknown-linux-musl, ext: tar.gz }
          - { target: aarch64-apple-darwin, ext: tar.gz }
          - { target: x86_64-apple-darwin, ext: tar.gz }
          - { target: x86_64-pc-windows-msvc, ext: zip }
    steps:
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: release-dist
          path: dist
      - uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2
        with:
          subject-path: dist/rb-${{ needs.verify.outputs.version }}-${{ matrix.target }}.${{ matrix.ext }}
          sbom-path: dist/rb-${{ needs.verify.outputs.version }}-${{ matrix.target }}.cdx.json

  # Tags the per-arch digests as one index only after the GitHub release exists.
  ghcr:
    needs: [verify, release]
    if: needs.release.result == 'success'
    runs-on: ubuntu-latest
    timeout-minutes: 10
    environment: release
    permissions:
      contents: read
      packages: write
      id-token: write
      attestations: write
      artifact-metadata: write
    env:
      GH_TOKEN: ${{ github.token }}
      VERSION: ${{ needs.verify.outputs.version }}
      SHA: ${{ needs.verify.outputs.sha }}
      PRERELEASE: ${{ needs.verify.outputs.prerelease }}
    steps:
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          pattern: digest-*
          path: digests
          merge-multiple: true
      - run: echo "$GH_TOKEN" | docker login ghcr.io -u "$GITHUB_ACTOR" --password-stdin
      - name: Create and tag the multi-arch index
        id: index
        run: |
          set -euo pipefail
          tags=(-t "$IMAGE:$VERSION")
          if [[ "$PRERELEASE" != "true" ]]; then tags+=(-t "$IMAGE:${VERSION%.*}" -t "$IMAGE:latest"); fi
          sources=()
          for f in digests/*; do sources+=("$IMAGE@$(cat "$f")"); done
          docker buildx imagetools create "${tags[@]}" \
            --annotation "index:org.opencontainers.image.source=https://github.com/${GITHUB_REPOSITORY}" \
            --annotation "index:org.opencontainers.image.description=rust-build server (rb serve): HTTP/1.1 + HTTP/2, TLS, WebSockets" \
            --annotation "index:org.opencontainers.image.version=${VERSION}" \
            --annotation "index:org.opencontainers.image.revision=${SHA}" \
            --annotation "index:org.opencontainers.image.licenses=MIT" \
            "${sources[@]}"
          digest=$(docker buildx imagetools inspect "$IMAGE:$VERSION" --format '{{json .Manifest}}' | jq -r .digest)
          echo "digest=$digest" >> "$GITHUB_OUTPUT"
      - uses: actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6 # v4.2.2
        with:
          subject-name: ${{ env.IMAGE }}
          subject-digest: ${{ steps.index.outputs.digest }}
          push-to-registry: true
          create-storage-record: false # storage records are org-only; this repo is user-owned
      - run: gh attestation verify "oci://$IMAGE:$VERSION" -R "$GITHUB_REPOSITORY"

  homebrew:
    needs: [verify, release]
    if: needs.release.result == 'success' && needs.verify.outputs.prerelease != 'true' && vars.HOMEBREW_TAP != 'false'
    runs-on: ubuntu-latest
    timeout-minutes: 10
    environment: release
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
          sparse-checkout: scripts/release
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: release-dist
          path: dist
      - name: Update kaeawc/homebrew-tap
        env:
          GH_TOKEN: ${{ secrets.HOMEBREW_TAP_TOKEN }}
          TAG: ${{ needs.verify.outputs.tag }}
          REPO: ${{ github.repository }}
          REQUIRE_TOKEN: "1"
        run: bash scripts/release/update-brew-formula.sh

  # Nightly dry run failed: open (or comment on) one tracking issue, as auto-mobile's nightlies do.
  nightly-alert:
    needs: [verify, build-cli, build-desktop, checksums, smoke-cli, smoke-desktop, docker]
    if: failure() && github.event_name == 'schedule'
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      issues: write
    env:
      GH_TOKEN: ${{ github.token }}
      RUN_URL: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}
    steps:
      - run: |
          set -euo pipefail
          title="Nightly release dry run is failing"
          n=$(gh issue list -R "$GITHUB_REPOSITORY" --state open --search "in:title \"$title\"" --json number --jq '.[0].number // empty')
          if [[ -n "$n" ]]; then
            gh issue comment "$n" -R "$GITHUB_REPOSITORY" --body "Still failing: $RUN_URL"
          else
            gh issue create -R "$GITHUB_REPOSITORY" --title "$title" --body "First failing run: $RUN_URL"
          fi
```

---

## 6. Scripts (`scripts/release/`)

### 6.1 `bump.sh`, run locally by the owner

```bash
#!/usr/bin/env bash
# Bump the version, commit, tag. Usage: scripts/release/bump.sh X.Y.Z[-pre] [--push]
# The two version strings: [workspace.package] in Cargo.toml and [package] in desktop/Cargo.toml.
set -euo pipefail
new="${1:?usage: bump.sh X.Y.Z[-pre] [--push]}"
push="${2:-}"
[[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]] || { echo "version must be X.Y.Z[-pre]" >&2; exit 2; }
[[ -z "$(git status --porcelain)" ]] || { echo "working tree is not clean" >&2; exit 1; }
[[ "$(git branch --show-current)" == main ]] || { echo "not on main" >&2; exit 1; }
git fetch -q origin main --tags
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || { echo "main is not level with origin/main" >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/v$new" >/dev/null; then echo "v$new exists (tags are immutable)" >&2; exit 1; fi

perl -0pi -e 's/(\[workspace\.package\]\nversion = )"[^"]*"/${1}"'"$new"'"/' Cargo.toml
perl -0pi -e 's/(\[package\][^\[]*?\nversion = )"[^"]*"/${1}"'"$new"'"/' desktop/Cargo.toml
for f in Cargo.toml desktop/Cargo.toml; do
  got=$(python3 -c 'import sys,tomllib; d=tomllib.load(open(sys.argv[1],"rb")); print(d.get("workspace",{}).get("package",{}).get("version") or d["package"]["version"])' "$f")
  [[ "$got" == "$new" ]] || { echo "failed to bump $f (got $got)" >&2; exit 1; }
done
# Refresh both lockfiles (workspace and path-crate versions) without upgrading anything.
cargo metadata --format-version 1 >/dev/null
(cd desktop && cargo metadata --format-version 1 >/dev/null)
if [[ -f CHANGELOG.md ]] && grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  perl -0pi -e 's/^## \[Unreleased\]\n/## [Unreleased]\n\n## ['"$new"'] - '"$(date -u +%F)"'\n/m' CHANGELOG.md
fi

cargo c && cargo lint && cargo t
(cd desktop && cargo check --locked)

git commit -qam "Release v$new"
git tag -a "v$new" -m "rust-build v$new"
if [[ "$push" == "--push" ]]; then
  git push --atomic origin main "v$new"
else
  echo "Tagged v$new. Publish with: git push --atomic origin main v$new"
fi
```

### 6.2 `package-cli.sh` and `package-desktop.sh`

```bash
#!/usr/bin/env bash
# package-cli.sh <label> <target>: target/<target>/dist/rb → dist/rb-<label>-<target>.{tar.gz,zip}
set -euo pipefail
label="$1" target="$2"
name="rb-${label}-${target}"
exe=rb; [[ "$target" == *windows* ]] && exe=rb.exe
stage="dist/stage/$name"
rm -rf dist/stage && mkdir -p "$stage"
cp "target/$target/dist/$exe" README.md LICENSE "$stage/"
cd dist/stage
if [[ "$target" == *windows* ]]; then
  7z a -tzip -bso0 "../$name.zip" "$name"
else
  COPYFILE_DISABLE=1 tar -czf "../$name.tar.gz" "$name" # no AppleDouble ._ files from macOS tar
fi
cd .. && rm -rf stage
```

```bash
#!/usr/bin/env bash
# package-desktop.sh <label> <version> <target>: Rune app → macOS .app zip / Linux tar.gz / Windows zip
set -euo pipefail
label="$1" version="$2" target="$3"
name="rb-desktop-${label}-${target}"
bin="desktop/target/$target/release/rb-desktop"
stage="dist/stage/$name"
rm -rf dist/stage && mkdir -p "$stage"
cp README.md LICENSE "$stage/"
case "$target" in
  *apple-darwin)
    app="$stage/rust-build.app"
    mkdir -p "$app/Contents/MacOS"
    cp "$bin" "$app/Contents/MacOS/rb-desktop"
    short="${version%%-*}"
    cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>rb-desktop</string>
  <key>CFBundleIdentifier</key><string>com.kaeawc.rust-build.desktop</string>
  <key>CFBundleName</key><string>rust-build</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${short}</string>
  <key>CFBundleVersion</key><string>${short}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET:-11.0}</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
    codesign --force --sign - "$app" # ad-hoc; Developer ID + notarization is phase 2
    (cd dist/stage && ditto -c -k --keepParent "$name" "../$name.zip")
    ;;
  *windows*)
    cp "$bin.exe" "$stage/"
    (cd dist/stage && 7z a -tzip -bso0 "../$name.zip" "$name")
    ;;
  *)
    cp "$bin" "$stage/"
    (cd dist/stage && tar -czf "../$name.tar.gz" "$name")
    ;;
esac
rm -rf dist/stage
```

### 6.3 `smoke-cli.sh` and `smoke-desktop.sh`

```bash
#!/usr/bin/env bash
# smoke-cli.sh <dist> <label> <version> <target>: verify, extract and exercise one rb archive.
# Runs under bash on Linux, macOS, Windows (Git Bash) and Alpine.
set -euo pipefail
dist="$1" label="$2" version="$3" target="$4"
name="rb-${label}-${target}"
case "$target" in
  *windows*) archive="$name.zip" exe=rb.exe ;;
  *) archive="$name.tar.gz" exe=rb ;;
esac
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd "$dist" && grep "  ${archive}\$" SHA256SUMS | sum -c -)

work="$(mktemp -d)"
case "$archive" in
  *.zip) 7z x -bso0 -o"$work" "$dist/$archive" ;;
  *) tar -xzf "$dist/$archive" -C "$work" ;;
esac
rb="$work/$name/$exe"

got="$("$rb" --version | tr -d '\r')"
[[ "$got" == "rb $version" ]] || { echo "rb --version printed '$got', expected 'rb $version'" >&2; exit 1; }

port=$((20000 + RANDOM % 20000)); tls=$((port + 1))
pids=()
cleanup() { local p; for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
wait_up() { for _ in $(seq 1 50); do "$rb" get -k "$1" >/dev/null 2>&1 && return 0; sleep 0.2; done; echo "server at $1 never came up" >&2; return 1; }

"$rb" serve --addr "127.0.0.1:$port" & pids+=($!)
wait_up "http://127.0.0.1:$port/healthz"
"$rb" get "http://127.0.0.1:$port/healthz"
"$rb" get -i "http://127.0.0.1:$port/api/info"
"$rb" loadgen -c 4 -d 2s "http://127.0.0.1:$port/healthz"

"$rb" serve --addr "127.0.0.1:$tls" --self-signed & pids+=($!)
wait_up "https://127.0.0.1:$tls/healthz"
"$rb" get -k -i "https://127.0.0.1:$tls/api/info" | tee "$work/info.txt"
grep -q 'HTTP/2.0' "$work/info.txt" || { echo "TLS + ALPN did not negotiate HTTP/2" >&2; exit 1; }

"$rb" cert --out "$work/certs"
test -s "$work/certs/cert.pem" && test -s "$work/certs/key.pem"
echo "smoke OK: $archive"
```

```bash
#!/usr/bin/env bash
# smoke-desktop.sh <dist> <label> <target>: launch the packaged app and wait for Rune's ready
# receipt ($RUNE_READY_DIR/<bundle id or exe name>.<pid>, written after the first on_ready).
set -euo pipefail
dist="$1" label="$2" target="$3"
name="rb-desktop-${label}-${target}"
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
work="$(mktemp -d)"
launch=()
case "$target" in
  *apple-darwin)
    archive="$name.zip"; ditto -x -k "$dist/$archive" "$work"
    exe="$work/$name/rust-build.app/Contents/MacOS/rb-desktop" ;;
  *windows*)
    archive="$name.zip"; 7z x -bso0 -o"$work" "$dist/$archive"
    exe="$work/$name/rb-desktop.exe" ;;
  *)
    archive="$name.tar.gz"; tar -xzf "$dist/$archive" -C "$work"
    exe="$work/$name/rb-desktop"
    launch=(xvfb-run -a)
    VK_ICD_FILENAMES="$(ls /usr/share/vulkan/icd.d/lvp_icd*.json | head -1)"; export VK_ICD_FILENAMES ;;
esac
(cd "$dist" && grep "  ${archive}\$" SHA256SUMS | sum -c -)

ready="$work/ready"; mkdir -p "$ready"
RUNE_READY_DIR="$ready"
[[ "$target" == *windows* ]] && RUNE_READY_DIR="$(cygpath -w "$ready")"
export RUNE_READY_DIR
"${launch[@]}" "$exe" & pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
for _ in $(seq 1 120); do
  if compgen -G "$ready/*" >/dev/null; then cat "$ready"/*; echo; echo "desktop ready: $archive"; exit 0; fi
  kill -0 "$pid" 2>/dev/null || { echo "app exited before it was ready" >&2; wait "$pid" || true; exit 1; }
  sleep 0.5
done
echo "no ready receipt within 60 s" >&2; exit 1
```

### 6.4 `notes.sh`

```bash
#!/usr/bin/env bash
# notes.sh <tag>: release notes = CHANGELOG section (if any) + commits since the previous tag + install/verify.
set -euo pipefail
tag="$1" version="${1#v}" repo="${GITHUB_REPOSITORY:-kaeawc/rust-build}"
prev="$(git describe --tags --abbrev=0 --match 'v*' "${tag}^" 2>/dev/null || true)"
url="https://github.com/${repo}/releases/download/${tag}"
if [[ -f CHANGELOG.md ]]; then
  awk -v v="$version" 'index($0, "## [" v "]") == 1 {f=1; next} /^## \[/ {f=0} f' CHANGELOG.md
fi
echo "## Changes${prev:+ since $prev}"
echo
git log --no-merges --format='- %s (%h)' "${prev:+$prev..}$tag"
cat <<EOF

## Install

\`\`\`sh
brew install kaeawc/tap/rust-build                     # macOS / Linux (stable releases)
docker pull ghcr.io/${repo}:${version}
curl -LO ${url}/rb-${version}-x86_64-unknown-linux-gnu.tar.gz   # or -musl, -aarch64-*, -apple-darwin, .zip on Windows
\`\`\`

## Verify

\`\`\`sh
curl -LO ${url}/SHA256SUMS && sha256sum -c --ignore-missing SHA256SUMS
gh attestation verify rb-${version}-x86_64-unknown-linux-gnu.tar.gz -R ${repo}
gh attestation verify oci://ghcr.io/${repo}:${version} -R ${repo}
\`\`\`

The desktop app (\`rb-desktop-*\`) is ad-hoc signed on macOS: after unzipping, run
\`xattr -dr com.apple.quarantine rust-build.app\`. On Linux it needs \`libxkbcommon-x11-0\`,
\`libwayland-client0\` and a Vulkan driver. musl builds are static but use musl's slower allocator;
prefer the gnu builds for \`loadgen\` and \`serve\` under load.
EOF
```

### 6.5 `update-brew-formula.sh`

```bash
#!/usr/bin/env bash
# Render Formula/rust-build.rb from dist/SHA256SUMS and push it to kaeawc/homebrew-tap.
# Env: TAG (vX.Y.Z), REPO (owner/name), GH_TOKEN (PAT, Contents:write on homebrew-tap),
#      REQUIRE_TOKEN=1 to fail instead of skipping when GH_TOKEN is empty, RENDER_ONLY=1 for tests.
set -euo pipefail
: "${TAG:?}" "${REPO:?}"
if [[ "${RENDER_ONLY:-0}" != 1 && -z "${GH_TOKEN:-}" ]]; then
  [[ "${REQUIRE_TOKEN:-0}" == 1 ]] && { echo "HOMEBREW_TAP_TOKEN is not set" >&2; exit 1; }
  echo "HOMEBREW_TAP_TOKEN is not set; skipping"; exit 0
fi
version="${TAG#v}"
url="https://github.com/${REPO}/releases/download/${TAG}"
sha_for() {
  local s; s=$(awk -v f="rb-${version}-$1.tar.gz" '$2 == f {print $1}' dist/SHA256SUMS)
  [[ -n "$s" ]] || { echo "no checksum for rb-${version}-$1.tar.gz" >&2; exit 1; }
  printf '%s' "$s"
}
mac_arm=$(sha_for aarch64-apple-darwin)
mac_x86=$(sha_for x86_64-apple-darwin)
lin_arm=$(sha_for aarch64-unknown-linux-gnu)
lin_x86=$(sha_for x86_64-unknown-linux-gnu)

render() {
cat <<EOF
class RustBuild < Formula
  desc "HTTP/1.1+2 server, client, load generator and TLS tunnel (rb)"
  homepage "https://github.com/${REPO}"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
      url "${url}/rb-${version}-aarch64-apple-darwin.tar.gz"
      sha256 "${mac_arm}"
    end
    on_intel do
      url "${url}/rb-${version}-x86_64-apple-darwin.tar.gz"
      sha256 "${mac_x86}"
    end
  end

  on_linux do
    on_arm do
      url "${url}/rb-${version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${lin_arm}"
    end
    on_intel do
      url "${url}/rb-${version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${lin_x86}"
    end
  end

  livecheck do
    url :stable
    strategy :github_latest
  end

  def install
    bin.install "rb"
    doc.install "README.md"
  end

  test do
    assert_match "rb #{version}", shell_output("#{bin}/rb --version")
  end
end
EOF
}

if [[ "${RENDER_ONLY:-0}" == 1 ]]; then render > rust-build.rb; exit 0; fi
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
git clone -q "https://x-access-token:${GH_TOKEN}@github.com/kaeawc/homebrew-tap.git" "$work"
mkdir -p "$work/Formula"; render > "$work/Formula/rust-build.rb"
cd "$work"
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Formula/rust-build.rb
if git diff --cached --quiet; then echo "formula unchanged"; exit 0; fi
git commit -qm "rust-build ${TAG}"
git push -q origin HEAD
```

---

## 7. Alternatives considered

| Option | Verdict |
| --- | --- |
| auto-mobile-style **prepare-release** dispatch (bump on a staging ref, build candidates, promote and tag) | Too heavy for two version strings and direct pushes. `bump.sh` plus the tag gate (on `main`, CI green on the SHA, versions match) gives the same "only publish what was verified" guarantee. |
| **Release PR** (release-plz / release-please) | Assumes PR flow and crates.io publishing; rust-build has `publish = false` and pushes directly. |
| **cargo-dist** | Owns its generated workflow and installs itself via `curl \| sh`. It doesn't cover the desktop workspace or the distroless image, and it differs from the owner's hand-rolled `scripts/release` style. |
| **goreleaser** (krit) | Go-centric. Its Rust support exists but adds a tool for what about 40 lines of shell do here. |
| **cross** / **cargo-zigbuild** | Not needed with native arm64 runners. zigbuild is the fallback if a glibc older than 2.35 becomes a requirement. |
| **docker/build-push-action** and friends | More convenient, but each needs an allow-list exception. `docker buildx` plus `imagetools` on the CLI covers push-by-digest, attestations and annotations. |
| **QEMU multi-arch image** | Slow for thin-LTO Rust. Native arm64 runners are free for public repos. |
| **GPG / minisign / Ed25519 manifest** | Key management and/or a third-party action. GitHub attestations cover provenance with keyless Sigstore. Revisit if offline verification is required. |
| **Rolling `nightly` tag / krit nightly tags** | Tags are immutable here, and krit's approach needs a PAT and fills the Releases page. Actions artifacts from the scheduled dry run are enough for a demo desktop app. |
