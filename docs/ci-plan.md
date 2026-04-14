# CI plan (deferred)

This documents the intended GitHub Actions workflow for exbar. Not yet
implemented — captured here so a future pass can land it without
re-brainstorming. Deferred during sub-project 1 of the staff-level
refactor; see `docs/superpowers/specs/2026-04-14-exbar-refactor-sp1-hygiene.md`.

## Trigger

- `push` to `main`
- `pull_request` against `main`
- tag push matching `v*.*.*` (for MSI publishing)

## Runners

`windows-latest` only — this is a Windows-only binary; Linux/macOS
cross-compilation is not a goal.

## Jobs

### lint

- `actions/checkout@v4`
- `dtolnay/rust-toolchain@stable`
- `Swatinem/rust-cache@v2`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`

### test

- `actions/checkout@v4`
- `dtolnay/rust-toolchain@stable`
- `Swatinem/rust-cache@v2`
- `cargo test --all-targets`

### build-msi (only on tag push)

Gated on `lint` and `test` success.

- `dotnet tool install --global wix`
- `wix extension add --global WixToolset.Util.wixext WixToolset.UI.wixext`
- `./scripts/build-msi.sh`
- `actions/upload-release-asset` for `target/wix/exbar-*.msi`

## Why deferred

SP1 scope was trimmed to "document CI, don't implement" at user request.
The hygiene pass (warnings, `[lints]`, `rustfmt.toml`) is sufficient for
local iteration. CI becomes valuable when:

- a second contributor joins, OR
- a release is cut and reproducible build provenance matters.

## Follow-up items

- **Review `clippy::pedantic` findings.** SP1 Task 7 held pedantic at
  `allow` because `warnings = "deny"` in `[lints.rust]` promotes clippy
  warnings to hard errors — an incompatible pair. The audit path: drop
  `warnings = "deny"` in favor of specific rustc lint groups
  (`unused`, `future_incompatible`), then flip pedantic back to `warn`
  and fix or opt-out each finding. When done on 2026-04-14 there was
  roughly 1 pedantic finding visible via `cargo clippy -- -W clippy::pedantic`;
  expect more as the codebase grows.
- Consider adding `cargo-deny` or `cargo-audit` jobs for dependency
  hygiene once CI is live.
- **MSI signing.** The current build produces an unsigned MSI. If we
  ship publicly via the tag-push workflow, a code-signing step must be
  added before `upload-release-asset` to avoid SmartScreen warnings.

## Pickup

When activating this plan, translate the jobs above into
`.github/workflows/ci.yml`. The existing `./scripts/build-msi.sh`
should work unchanged on `windows-latest` — it already installs its
own prerequisites via `dotnet tool`.
