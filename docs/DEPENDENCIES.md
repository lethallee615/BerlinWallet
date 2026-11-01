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

## Alpha dependency decisions

The alpha dependency queue was cleared on 2026-10-31:

- #54 merged the rust-minor-patch group: `serde_json` 1.0.154, `tokio` 1.54.1,
  `uuid` 1.29.0, and `libc` 0.2.191. CI passed after the rebase.
- #53 merged the GitHub Actions group: `actions/checkout@v7` and
  `actions-rust-lang/setup-rust-toolchain@v2` in both CI and the release
  workflow. CI passed.
- #55 (`rand` 0.8.8 -> 0.10.3) was closed for the alpha. It is a transitive
  lockfile bump across the Fedimint-pinned `soketto`/`jsonrpsee`/`iroh` graph,
  not a wallet-code requirement or a security fix. Move it as one coordinated
  dependency change later.

The 12 open alerts and their reachability review are above. The advisory check
stays non-blocking for alpha; remove the exceptions and make it blocking before
beta or production.

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

## Open alert triage (alpha)

GitHub reports 12 open dependency alerts against `Cargo.lock`: three high, four
medium, and five low. This is a reachability review for the alpha, not an
audit. The RustSec IDs and reasons for the accepted exceptions are in
`deny.toml`; the advisory CI step remains non-blocking for alpha.

The three high alerts are:

| Alert | Path | Why it is not an alpha blocker |
|---|---|---|
| `GHSA-5j98-2g5x-46v6` — `hickory-resolver` obscures DNSSEC validation failures | `iroh`/Fedimint connector DNS | The wallet does not call the `Resolver::lookup*` DNSSEC APIs or claim DNSSEC validation; federation endpoints are configured by the user. |
| `GHSA-3v94-mw7p-v465` — `hickory-proto` NSEC3 unbounded loop | `iroh`/Fedimint connector DNS | DNSSEC/NSEC3 validation is not enabled on the wallet's resolution path. |
| `GHSA-82j2-j2ch-gfr8` — `rustls-webpki` panic on a malformed CRL | `rustls`/Fedimint HTTP and relay | The wallet does not parse or configure CRLs; it uses the default webpki certificate path. |

The remaining alerts are in transitive `rustls`, `rustls-webpki`, `hickory`,
`lru`, and `anyhow` paths. They are tracked in `deny.toml` with the same
reachability caveat. Clear them as upstream Fedimint/iroh releases permit; make
the advisory check blocking before beta/production. Do not treat this table as a
security guarantee.
