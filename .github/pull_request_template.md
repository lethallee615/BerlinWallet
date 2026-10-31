## What

What changed, in one paragraph. Name the crate/module and the behavior.

## Why

Closes #
Refs #

## Test

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] New or changed behavior has a test that fails without this change

Test names and what they pin:

## Live gate

- [ ] Not needed for this change
- [ ] Passed — date, backend, federation, flow, result, evidence

A merged PR is not the same as a verified behavior. If a live run is needed,
say exactly what was run and what was observed; if it is still pending, say so
and leave the issue open.

## Risk and rollout

Anything that can lose funds, change on-disk state, or break restart. If none,
say none.

## Checklist

- [ ] An issue exists and is linked
- [ ] `docs/STATUS.md` updated if an MVP row or backend coverage changed
- [ ] `docs/JOURNAL.md` entry added **after** merge, newest first, if the change
      has a non-obvious why
- [ ] Docs that made a now-false claim are corrected (`README.md`,
      `ROADMAP.md`, `AGENTS.md`, `docs/API-VERIFICATION.md`)
