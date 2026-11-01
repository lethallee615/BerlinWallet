# Dependency update policy

Dependabot is configured in [`.github/dependabot.yml`](../.github/dependabot.yml).

## Cargo

- Weekly, Monday 06:00 UTC, against `main`.
- `versioning-strategy: lockfile-only`: the exact pins in `Cargo.toml` remain
  deliberate; Dependabot updates the lockfile for compatible releases.
- Minor and patch updates are grouped into one PR; at most five Cargo PRs are
  open at once.
- Security updates are still proposed by Dependabot, subject to the ignores
  below.

## Fedimint patch pin

The workspace has a large `[patch.crates-io]` block pointing every Fedimint crate
at git tag `v0.12.0`. Dependabot cannot update one patched crate independently
without producing a mixed Fedimint generation or a lockfile that fights the
patch. `fedimint-*` is therefore ignored by Dependabot; the patch tag is bumped
as one coordinated dependency change. `deny.toml` allows only that git source.

## `lru` exception

GitHub security alert GHSA-rhfx-m35p-ff5j / RUSTSEC-2026-0002 covers
`lru < 0.16.3`. The lockfile contains:

- `lru 0.12.5`, required by `iroh-relay 0.35.0` (`lru ^0.12`);
- `lru 0.13.0`, required by `pkarr 3.10.0` (`lru ^0.13.0`);
- `lru 0.16.4` and `lru 0.18.5`, which are not in the vulnerable range.

`iroh` and `pkarr` arrive through the Fedimint v0.12.0 patch. The fixed
`lru 0.16.3` cannot satisfy the older semver requirements:

```text
$ cargo update --dry-run -p lru@0.12.5 --precise 0.16.3
error: failed to select a version for the requirement `lru = "^0.12"`
required by package `iroh-relay v0.35.0`
...
$ cargo update --dry-run -p lru@0.13.0 --precise 0.16.3
error: failed to select a version for the requirement `lru = "^0.13.0"`
required by package `pkarr v3.10.0`
...
```

The `IterMut` soundness advisory is a low-severity library issue; the wallet
does not call `lru::IterMut` directly, and this exception is documented rather
than silently ignored. Dependabot ignores `lru < 0.16.3` and the equivalent
advisory is listed in `deny.toml`. Remove both when the Fedimint/iroh/pkarr graph
can move to `lru >= 0.16.3`.

## GitHub Actions

Weekly, grouped, at most three open PRs. Actions are pinned to major-version
tags; a review must still check the action's source and permissions before
merge.

## Reviewing a Dependabot PR

1. Check the advisory/security reason and whether the affected path is reachable.
2. Run `cargo fmt --check`, strict clippy, and `cargo test --workspace`.
3. Run `cargo deny check licenses sources bans`; advisories remain visible via
   the non-blocking advisory job.
4. For a major update or a change to the Fedimint graph, rerun the relevant
   devimint live gate before merging.
