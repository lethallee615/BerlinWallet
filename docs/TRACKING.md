# How work is tracked

This project is maintained by humans and AI agents. Both need the same three
things: a queue of work, a definition of done, and evidence that a claim is
true. The repository uses GitHub Issues for the queue and checked-in documents
for the definition and the evidence.

## Source of truth, in order

| Rank | Artifact | Answers | Must not contain |
|---|---|---|---|
| 1 | Source and tests | What the code actually does | Aspirations |
| 2 | GitHub Issues | What is left, who owns it, why it matters | Unscoped brain-dumps |
| 3 | `docs/STATUS.md` | Where each MVP requirement stands today | New claims without evidence |
| 4 | `ROADMAP.md` | What “done” means for the MVP and later stages | The live queue |
| 5 | `docs/API-VERIFICATION.md` | What was verified, on which pinned version, with which result | Guesses presented as facts |
| 6 | `docs/JOURNAL.md` | Why a decision was made, written after merge | Open work or a task list |
| 7 | `AGENTS.md` | Durable working context for agents | Ephemeral status |

When two artifacts disagree, the higher rank wins. Fix the lower one in the same
PR.

## The work queue: issues, labels, milestones

There is no Projects board. A board would be a second queue that has to be kept
in sync, and the issue API is what `gh` and agents can read reliably.

Every issue has:

- one `area:*` label (`core`, `cli`, `ffi`, `chain`, `build`, `ci`, `docs`);
- one `type:*` label (`bug`, `feature`, `test`, `docs`, `chore`);
- one `priority:*` label;
- one milestone (`MVP`, `Hardening`, `Post-MVP`);
- optionally `needs:live-test` and/or `blocked:upstream`.

Priority:

- `P0` — funds are at risk or the primary flow is broken. Drop other work.
- `P1` — MVP blocker. The MVP is not done until it closes.
- `P2` — hardening. Real, but after the MVP.

The issue forms in `.github/ISSUE_TEMPLATE/` enforce the fields. Do not open an
issue outside them unless it is a duplicate report.

## Status vocabulary

`docs/STATUS.md` uses exactly these values:

| Value | Meaning |
|---|---|
| `todo` | Not started. |
| `in-progress` | Code exists but is not verified end to end. |
| `in-review` | A PR is open; the issue is not closed. |
| `live` | Exercised end to end on a live federation (devimint or Mutinynet). |
| `done` | Implemented, tested, and documented; no live gate is needed or it passed. |
| `at-risk` | Implemented and previously live, but a known open issue threatens it. |
| `blocked` | Waiting on an upstream Fedimint/BDK change. |

A merged PR moves a row to `in-progress` or `done` only if the acceptance
criteria were met. “Merged” is not a status.

## The lifecycle of a change

1. **File or claim an issue.** One issue per PR-sized unit. If you cannot name
   the testable outcome, the issue is not ready.
2. **Branch from `main`.** `feat/`, `fix/`, or `chore/` prefix.
3. **Open a PR.** Use the template. Link the issue with `Closes #N` or
   `Refs #N`. Say what is verified and what is still pending.
4. **Pass the gate.** `cargo fmt --check`, strict clippy, workspace tests, and
   `python3 scripts/check_status.py`. A live gate is named in the issue if one
   is needed.
5. **Merge with squash.** `main` has linear history; merge commits are rejected.
6. **Update the documents.** If an MVP row or backend coverage changed, edit
   `docs/STATUS.md`. If the change has a non-obvious why, add a `docs/JOURNAL.md`
   entry at the top after merge.
7. **Close the issue.** If something remains, file a new issue instead of leaving
   an open thread in prose.

## The agent protocol

An agent starting a session should read, in this order:

1. `AGENTS.md` — scope, crate map, pinned versions, hard rules.
2. `docs/STATUS.md` — the current snapshot.
3. The open issues, filtered by priority: `gh issue list --state open --label priority:P0`.
4. `docs/JOURNAL.md` and the relevant `docs/API-VERIFICATION.md` sections.

Rules for agents:

- **Claim work by issue.** Do not start a change that has no issue; file one
  first. Do not close an issue without either a test or a live gate named in it.
- **Cite the source.** Every claim about an API or behavior names a file and
  line, a docs.rs page, or a live observation. Mark guesses as guesses.
- **Do not invent status.** `docs/STATUS.md` is updated from evidence, not from
  intention. If a gate has not run, the row is `in-progress`, not `done`.
- **Do not duplicate.** Before filing, search open and closed issues.
- **Keep the journal for why, after the fact.** It is not a scratchpad.
- **Correct drift in the same PR.** If a change makes `README.md`, `ROADMAP.md`,
  or `AGENTS.md` false, fix them there.

## Audit commands

```bash
# The queue, grouped by priority
gh issue list --state open --label priority:P0
gh issue list --state open --label priority:P1
gh issue list --state open --label priority:P2

# The snapshot, validated offline (CI) or against GitHub (local)
python3 scripts/check_status.py
python3 scripts/check_status.py --online

# What changed recently and which issue it closed
gh pr list --state merged --limit 20
git log --oneline -20
```

`check_status.py --online` fails when an open P0/P1 issue is missing from
`docs/STATUS.md`, so the queue and the snapshot cannot silently diverge.

## What this model deliberately avoids

- A Projects board as a second source of truth.
- A generated changelog before the first tag.
- Status in chat, commit messages, or PR descriptions alone.
- “Done” without an evidence link.
