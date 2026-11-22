# Dependency follow-ups (2026-11-23)

Items that are intentionally deferred. Each entry names the review trigger so a
temporary exception does not become permanent.

## chrono → time / jiff

- Advisory: RUSTSEC-2026-0543, "chrono is unmaintained; all versions affected",
  no fixed release.
- Current handling: `deny.toml` ignores this advisory with a comment.
- Review trigger: before the next dependency sweep, or when a new advisory names
  chrono with a fixed version. Migrate the timestamp formatting, RFC 3339
  parsing and WebDAV date handling to `time` or `jiff` first, then delete the
  ignore entry.

## pin-utils

- Advisory: RUSTSEC-2026-0441, unmaintained, no successor release.
- Current handling: nothing to configure. `cargo audit` reports it as a warning
  (exit 0); `cargo deny` only checks unmaintained advisories for workspace
  crates (`unmaintained = "workspace"`), and pin-utils is transitive through
  aws-smithy-http, so no ignore entry is needed.
- Review trigger: if deny's policy widens to transitive crates, or the parent
  crate drops it.

## Frontend dev dependencies

- `npm audit --audit-level=moderate` runs in CI. Dev-only packages (vitest,
  happy-dom, eslint toolchain, vite) must stay on patched releases; they do not
  ship in `static/app`.
- Review trigger: every CI dependency-audit failure; upgrade rather than ignore
  whenever the advisory lists a fixed version.
