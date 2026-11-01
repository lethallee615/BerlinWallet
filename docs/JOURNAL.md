# Journal

## 2026-10-31 — The REPL handles paste, shell passthrough, and stray keys

The development REPL input path is cleaned up. Lines are normalized for carriage
returns and bracketed-paste markers; a single misplaced keystroke is ignored;
`!<command>` and `shell <command>` pass through to `sh -c`; and unknown
multi-character input gets one short hint rather than an error chain.

The tracing writer now redraws `berlin-wallet> ` after a background log only
while the REPL is waiting for input. That removes the need for an extra Enter
when a sync or task log appears over the prompt, without injecting a prompt in
the middle of command output.

Startup/`init` now peek at the current unused BDK address instead of revealing
the next one, so repeated sessions show the same address; the explicit `address`
command still reveals and persists a new one.

PTY checks covered two commands pasted in one write, stray `s`, `!echo` and
`shell echo`, a background chain-sync WARN redrawing the prompt, and two `init`
runs printing the same address.
## 2026-10-31 — Withdrawal quote and change semantics corrected

Two long-standing withdrawal questions are resolved.

The 224 msat quote/charge mismatch was a gross-up bug: walletv2's
`send_fee_quote` expects the wallet output amount **including the on-chain miner
fee**, but the withdrawal code passed only the recipient value. Both quote paths
now pass `value + chain_fee`, and a regression test pins the gross-up. A live
devimint withdrawal reconciled exactly: 50,000 sat + 228 sat on-chain + 101,600
msat federation = 50,329,600 msat, which matched both `operation-fees` and the
observed ecash balance delta.

`change_outpoint_range` was not a set of phantom Bitcoin txids. It is a
`fedimint_core::OutPointRange` (fedimint transaction id plus output indices) for
the mintv2 ecash change. `await_primary_bitcoin_module_output` waits for those
federation outputs; they are not expected on the Bitcoin chain. The live send
range used a fedimint txid distinct from the actual on-chain withdrawal txid.
The wait is an ecash-change wait, not an on-chain confirmation, and now has a
type-proof test and corrected docs.
## 2026-10-31 — The mnemonic and backups are encrypted

The mnemonic file is now an age passphrase-encrypted blob. New wallets prompt for
a wallet passphrase; automation can set `BERLIN_WALLET_PASSPHRASE`. A legacy
plaintext mnemonic is parsed once with the supplied passphrase and immediately
rewritten encrypted. The BIP39 seed still uses the empty BIP39 passphrase; this
is an at-rest storage passphrase, not a second BIP39 passphrase.

Backups changed from an unencrypted directory copy to a single age-encrypted
tar. The archive carries a versioned `.berlin-wallet-backup.json` manifest, the
encrypted mnemonic, `chain.json`, `bdk.sqlite` plus WAL/SHM, and the RocksDB
directory. Backup and restore use separate temp paths and rename only complete
results; import validates the tar paths, manifest version, and required files
before touching the target. Wrong backup passphrases and tampering fail
authentication. The live RocksDB/SQLite files remain plaintext and rely on
full-disk encryption; the archive is the application-level at-rest protection.

A live CLI check copied an existing devimint data directory, migrated its
plaintext mnemonic, backed it up to a 1.5 MB age archive, restored it into a
fresh directory, and reopened it with the wallet and backup passphrases. The
core tests cover migration, wrong passphrases, tampering, sidecars, stale temps,
existing destinations, plaintext-source refusal, and locked databases.
## 2026-10-31 — Quiet shutdown without hiding task failures

The CLI tracing subscriber is now reloadable. On quit, after the REPL stops
reading commands but before `Console` shutdown, it appends `fm::task=off` to the
`EnvFilter`; the fedimint task-cancellation lifecycle messages disappear while
`fm::task` remains visible during normal wallet operation. A live
`RUST_LOG=info` quit with the funded deposit/output scanner present exited 0
with no `fm::task` WARN or ERROR after the prompt.

The filter is only narrowed at shutdown, so a genuine task panic during normal
operation is still reported. This closes the last known cosmetic shutdown-noise
class from #10.
## 2026-10-31 — `watch` reports progress instead of hanging silently

`watch <deposit-id>` now prints a stderr progress line immediately and every ten
seconds. Each line names the deposit, current row state, starting event-log
position, last observed federation position, funding txid, operation id, and
elapsed time. The final row stays on stdout, so scripts are unaffected.

The existing Ctrl+C path was rechecked against a devimint-funded deposit: the
initial and 10 s lines appeared, SIGINT printed the interrupt hint and the
watch-cancelled message, and the REPL returned to its prompt without killing the
process. `watch` remains the deposit-claim wait; withdrawals use
`send-withdraw`/`status`.

The formatter is unit-tested, and the CLI test count is now seven.
## 2026-10-31 — The crash matrix runs in CI

The crash matrix is now a deterministic test module instead of a manual
Mutinynet checklist. Each test performs a partial operation, drops the journal,
reopens the same database, and asserts the exactly-once transition.

The CI suite covers address-shown before funding, funding broadcast before
claim, a restart during confirmation, a restart after `Done`, the
`send()`-before-operation-id window via boot adoption, the final-send-state
before `Broadcast`, and a change-output timeout that leaves the row `Submitted`
for the next reconciliation. The confirmation test replays the claim after
restart and proves one binding, one `Done` row, and one credit; a later row
cannot steal the outpoint.

One window cannot be reproduced with the in-memory journal: the real network
broadcast happens before BDK persistence and the journal's `funding_txid` write.
That test is explicitly `#[ignore]`d as devimint-only, and the list lives in
`crash_matrix::DEVIMINT_ONLY_WINDOWS` so CI fails if the exception is removed
from the documented matrix. The live `fault::crash_point` hooks gained
`after_deposit_address` and `after_deposit_done` alongside the existing deposit
broadcast, withdrawal send, and broadcast hooks.

These tests prove the app-level recovery transitions, not the federation's
underlying exactly-once behavior; the live §27 evidence remains necessary for
that. Tests: 111 core pass + 1 ignored, 6 CLI, 5 FFI.
## 2026-10-31 — Restore assertions and a balance-settlement race

The restore contract is now asserted rather than only described. A small
`restore_policy` module owns the exact warning text, the `history_unavailable`
decision, and `is_balances_only()`. The CLI prints the operation-log flag,
history flag, journal counts, and all three warnings, and fails if a fresh
restore is not balances-only. The FFI `RestoreView` carries the same warning
strings; its restore path also enforces the balances-only result.

The live devimint gate found a real bug first: recovery reconstructed
`99,577,856 msat`, but `restore` printed `0 msat`. mintv2 recovery adds a pending
output state machine to issue the reconstructed notes; `wait_for_all_recoveries`
does not wait for that issuance, and `recover_client` reopened and returned
immediately. `recover_client` now calls
`Client::wait_for_all_active_state_machines()` after reopening, and the same
gate then recovered the ecash and BDK balances correctly.

The gate also created a funded, unclaimed 50,000 sat deposit before restoring.
The restored client showed the settled 99,577,856 msat, an empty operation log
and journal, and all warnings. A new `request-deposit 10000` reused the same
walletv2 address in this run, but the ecash balance did not change and the
stranded UTXO remained unspent after ten more blocks. Address reuse is not a
re-claim; the warning's promise remains accurate.

Tests: 103 core, 6 CLI, 5 FFI (including exact warning-text assertions).
## 2026-10-31 — Electrum is live on devimint

Electrum was the last chain backend with unit coverage only. A fresh devimint
session exposed electrs on `127.0.0.1:60401`; a wallet initialized with
`electrum:tcp://127.0.0.1:60401` synced a 0.1 BTC funding address, funded and
claimed a 100,000 sat deposit, and withdrew 50,000 sat to an external regtest
address.

The deposit used Electrum for the BDK sync and funding transaction. The claim
completed at `Done` with 99,577,856 msat credited; the actual operation fee was
106,144 msat against the fee-preview estimate. The withdrawal reached
`Broadcast { txid: bf1c3334… }`; after a restart the row still had that txid and
the transaction had one confirmation. Restarts with no chain-source argument
kept using the stored `chain.json`, and a bare `init --network regtest` printed
the Electrum source, so the persistence round trip is covered.

The reorg path was not attempted through Electrum: the wallet cannot drive
`invalidateblock`. The deterministic bitcoind reorg gate from #18/§47 remains the
reorg evidence, and the ancestry/rebuild logic is shared above the backend. Full
scan/restore on Electrum and injected transient failures were not re-run in this
gate. A non-fatal `fm::client` write-write conflict warning appeared while setting
the claim outcome; the operation still completed once with the expected credit.
## 2026-10-31 — Deposit fee preview before funding

`request-deposit` and `deposit` now show the on-chain claim fee and the best
available federation operation-fee estimate before any BTC moves. The estimate
sums the walletv2 input fee and the mintv2 output fee from the federation's
client configs; it is explicitly labelled an estimate.

The first implementation used only the mintv2 consensus and quoted 100 msat
against a live actual of 104,096 msat. That was technically the mintv2 output
fee but not the full claim cost, so the preview now includes the walletv2 input
consensus too. On the final live gate the quote was 100,100 msat and the actual
was 103,072 msat. The remaining gap is change/dust inside the federation
transaction, which the fee consensus does not expose. The actual is stored and
`status` prints the mismatch instead of replacing the quote with a silent
"actual" or treating an unknown fee as zero.

The journal gained `quoted_operation_fee` and `operation_fee`. Previously the
operation fee was only a temporary value used to compute `net_credit`; it is now
recorded. Both fields are `#[serde(default)]`, so older deposit rows load with
`None` and the CLI says the quote is unavailable. The FFI `DepositView` exposes
the quoted claim fee, quoted operation fee, and actual operation fee additively.

Live gate: 100,000 sat deposit on devimint's bitcoind backend. Previewed claim
fee 316 sat, operation estimate 100,100 msat, estimated net 99,583,900 msat.
Actual: claim fee 316 sat, operation fee 103,072 msat, net credit 99,580,928
msat; the mismatch was printed. Tests are 99 core, 5 CLI, and 4 FFI.

This is a plain-language record of what Berlin Wallet is, what was built, and why.
It is not API documentation and it is not a changelog of every commit. The source
and `docs/API-VERIFICATION.md` remain authoritative for current behavior.

## 2026-10-31 — Lightning send and receive are wired

The API verification in §48 became the implementation. `lnv2` is registered with
`LightningClientInit::default()`, and `lightning.rs` follows the deposit and
withdrawal discipline rather than inventing a second one.

A Lightning row is written **before** the operation, with its id in
`custom_meta.id`. A send first checks `get_invoice_send_status`; an invoice that
has any operation is refused rather than re-sent. The operation id is persisted
after `send`/`receive`, and the final state is driven by the lnv2 state machine.
The adoption pass scans the operation log and matches `custom_meta.id` (or the
operation id), so a restart resumes and updates a row without re-sending.
`ln-status` and `ln-list` refresh pending rows on demand; the 60 s adoption tick
does the same in the background.

Fees are kept as two layers: the gateway fee from `RoutingInfo`/the operation
meta, and the federation `FeeQuote` from `send_fee_quote`/`receive_fee_quote`.
The send quote total matched the balance drop exactly (1,007,104 msat for a
1,000 sat invoice: 5,000 gateway + 2,104 federation). The receive quote is
conservative by a few hundred msat; the balance is the truth.

The live gates on devimint: a paid send (`Succeeded`, preimage, LND invoice
`SETTLED`), a paid receive (`Claimed`), a receive paid while the client was
offline and claimed after restart, an expired receive (`Expired`), an expired
send (`Failed`), and a duplicate settled send refused. The expired receive uses
a local invoice-deadline fallback because the upstream final stream lagged.

Two limits are deliberate. A client data directory created before lnv2 was
registered has no lnv2 module in its stored module config; it needs a fresh
join/restore. And a gateway-side routing failure (as opposed to an expired
invoice) was not exercised; signet is still a live-gate gap.

## 2026-10-31 — lnv2 client API verified before implementation

Lightning was the next MVP gap, but the `fedimint-lnv2-client` surface had never
been checked against the pinned version. That check is now in
`docs/API-VERIFICATION.md` §48; no lnv2 code is registered yet.

The important findings:

- `LightningClientInit::default()` needs no gateway URL. It builds
  `RealGatewayConnection` with `GatewayApi::new(None, connector_registry)` and
  discovers the federation's vetted gateways at runtime. Registering the module
  is the whole client-side wiring.
- The v2 module kind is `lnv2`, distinct from the v1 `ln` module. `fedimint-cli`
  registers both; this wallet should register only v2, because the live
  federation exposes `lnv2` and no `ln`.
- `send`/`receive` return an `OperationId` (and `receive` also the Bolt11
  invoice), with final-state enums and update streams. `get_invoice_send_status`
  is the anti-double-payment check; `send` itself rejects a duplicate operation
  id derived from `(invoice, 0)`.
- The federation fee and the gateway fee are separate. `send_fee_quote` and
  `receive_fee_quote` quote the federation transaction only; the gateway's fee
  is in `RoutingInfo`/the operation meta. Client limits are 1.5% + 100 sat for
  send and 0.5% + 50 sat for receive; invoice expiry is capped at one day.
- Gateway registration is an admin/guardian action; clients only list and
  select. A gateway needs both inbound and outbound Lightning liquidity plus
  on-chain funds for channels. The protocol is not trustless: the federation is
  custodial for ecash, the gateway is a Lightning counterparty that can
  censor/delay and sees payment metadata, and routing nodes see the payment.
- Events (`payment-send`, `payment-send-update`, `payment-receive`) and the
  `custom_meta` field give the journal the same adoption hooks used by
  deposits/withdrawals.

Live check: the devimint v0.12.0 federation exposes `lnv2` as module 0. The
runtime gates (paid, failed, expired, interrupted invoices, gateway selection,
refund timing) belong to #23.

## 2026-10-31 — First-class on-chain send

The BDK wallet could reveal addresses and fund federation deposits, but there was
no arbitrary on-chain send. `send <address> <sat> <sat/vb>` now builds the
transaction, prints an exact quote before signing (amount, fee, total debit,
change, input count, txid), signs with the BIP84 descriptors, broadcasts through
the selected `ChainBackend`, and records the result in the journal. `list-onchain`
shows the journalled sends and the wallet graph's transactions with confirmation
state.

The quote and the journal row are deliberately separate. The quote is derived
from the finalized PSBT, so the fee is exact before signing; the journal row is
written only after a successful broadcast. A crash between broadcast and the
journal insert leaves the transaction in the BDK graph (and in `list-onchain`'s
wallet section) but without the audit row; that window belongs to the crash
matrix (#25), not to the send itself.

Live gates on devimint (bitcoind backend): a 10,000 sat send at 2 sat/vB paid a
281 sat fee with 9,989,719 sat change, confirmed, survived restart, and reorged
cleanly. `invalidateblock` at the send's height made the wallet rebuild from the
orphaned checkpoint and report the send unconfirmed; re-mining the mempool
returned it to one confirmation. The journal row stayed `Broadcast` throughout.
A signet run remains open as #38.

## 2026-10-31 — Ctrl+C cancels; Ctrl+Z no longer strands the database lock

`quit` sent the shutdown flag but did not wait for the sync tasks or consume the
client handle. The process exited with the client's background tasks still
logging, and a suspended process held the RocksDB lock until it was killed. The
next launch then blocked on a lock whose owner looked idle.

The CLI now keeps the task handles. On exit it sends the shutdown flag, drains
the deposit observers, waits (with a five-second timeout) for the BDK, fedimint,
and adoption tasks, and calls `ClientHandle::shutdown` before returning. The
client's own tasks are stopped by that call, so the next `run` can open the same
data directory immediately.

Signals:

- SIGINT is trapped. At the prompt it prints a hint and keeps the session alive;
  during `watch` it cancels the foreground wait and returns to the prompt. The
  background observer and the 60-second adoption pass keep the row moving.
- SIGTSTP (Ctrl+Z) is trapped and refused with a message. Suspending an open
  wallet keeps the RocksDB lock, so the wallet tells the user to `quit` instead.

Gate: a PTY test sent Ctrl+C, Ctrl+Z, and `quit`; both hints appeared, the
process exited 0, and a second `run` opened the same data directory immediately.
A `quit` at `RUST_LOG=debug` produced no WARN or ERROR lines after shutdown.

## 2026-10-31 — One row per deposit in `list-deposits`

The adoption fix stopped new duplicate rows, but the live data directory still
had a `Requested` and an `Adopted` row for one claim. They share the funding
txid and operation id; the balance was correct, yet listing both made one
deposit look like two.

`list-deposits` now groups rows by funding txid (one deposit output per funding
tx), then operation id, then row id. Within a group it shows the `Done` row
first, then the `Requested` row, then the most recently updated one. A genuine
adopted-only deposit still appears; a duplicate of a requested deposit does not.
`status <id>` still resolves the hidden row directly, so the claim owner remains
auditable.

The FFI `list_deposits` uses the same collapse, so a GUI does not have to
reimplement it.

## 2026-10-31 — Adoption and the watcher stop racing on a deposit claim

The live gate for #14 succeeded at the federation layer: a 100,000 sat deposit
produced a walletv2 Receive operation with the expected outpoint and outcome
`Success`, and the ecash balance rose by 99,577 sat. The requested journal row
still said `AddressShown`.

`adopt_orphan_claims` and the per-deposit observer both act on the same
successful claim. Adoption only checked the claim-ownership map, so when the
requested row had not bound yet it inserted an `Adopted` row. That marked the
outpoint bound; the observer's scan filtered bound outpoints, so it could no
longer close the requested row. The result was two rows for one deposit: a
correct `Adopted` row and a stranded `Requested` one.

Adoption now matches an open requested row by address script and amount first,
binds or closes that row with the claim's figures, and only creates an
`Adopted` row when no open row matches. `close_deposit_done_if_open` records the
operation id as well as the gross and claim fee, so the repaired row is
auditable. The observer scans all claims, not only unbound ones, so it can close
its row even when adoption won the bind. `list-deposits` prints every row,
including `Adopted`, because the previous CLI could not show the row that had
taken the claim.

## 2026-10-30 — A funded deposit is followed in the background

`deposit` and `fund-deposit` used to broadcast the funding transaction and
return, leaving the row at `AddressShown` until someone ran `watch`. That is now
a background observer: a funded row is tracked from the moment the funding
transaction exists, the observer is resumed from the journal on the next
`run`/`open`, and shutdown drains it before a client handle is consumed.

The observer is not the claim. `fedimint-walletv2-client` runs its own output
scanner and claims a received output autonomously; `await_receive` only reports
the result. The observer calls it, binds the claim to the row, and exits when the
row is terminal. A per-row active set keeps one observer per deposit, and
`DepositWatchers::wait_all` drains the handles so the FFI `Arc::try_unwrap` check
does not see a lingering client reference.

Three gaps made the original report (#14) hard to read, and are closed:

- The funded outpoint is recorded from the transaction that was just built, not
  only when a claim binds. A funded row is observable immediately and restart
  recovery does not depend on a chain scan.
- An amount that does not cover the federation's claim fee is refused before
  broadcast. Upstream `receive_output` returns `None` in that case without
  logging, so the deposit would have been spent and silently never claimed.
- A funded row still unclaimed after five minutes emits a WARN naming the
  deposit, address, amount, and funding txid. That does not prove the federation
  failed to observe the transaction; it names the condition the old code hid.

What this does not fix: if the federation never sees the funding transaction
(wrong node, dead observer, or an unclaimable output), the deposit still does not
complete. The WARN and the recorded outpoint make that state legible; the live
gate for #14 still has to establish which layer is failing.
## 2026-10-30 — Network selection is explicit and fail-closed

The network used to come from the federation's block-1 chain-id, with an
unknown chain-id silently mapping to `Network::Regtest`. The fallback is
upstream server behaviour and is convenient for devimint, but in a wallet it
changes address prefixes and BIP84 descriptors without saying so. Addresses are
the one thing a user is asked to verify; a wrong network there is exactly the
mistake the wallet must not make quietly.

`network_from_chain_id` now returns a `Result` and names the offending chain-id.
The escape hatch is a recorded choice, not a fallback:

- `init` requires `--network <mainnet|testnet|signet|regtest>` and writes
  `network.json` (versioned, 0600, atomic rename).
- `run`/`open` read it, derive the chain-id network, and refuse a mismatch.
- An unknown chain-id is accepted only when the recorded choice is `regtest`,
  which is the explicit devimint/private-chain opt-in. A different network needs
  a fresh data directory; `init_network` refuses to change an existing record.
- The FFI makes the choice part of the API: `init(network)` and
  `restore(..., network)`, and `InitView` reports it. `join`/`open` require the
  recorded file, so an app cannot skip the decision by calling a different
  entry point.

This also fixed an older argument bug: `init` read its chain source from
`args[3]`, so `berlin-wallet init esplora:...` was ignored. The chain source is
the first positional after the command.
## 2026-10-30 — The devShell carries its own Go and an optimized C flag

The Nix shell could not build the workspace on a machine whose `target/` had
been configured outside it. Two independent causes, both shell-specific:

- `aws-lc-sys` configures its CMake build from source, and that path can shell
  out to `go` for code generation. `go` was not in the devShell, so CMake
  stopped at `cmake/go.cmake` with "Could not find Go".
- The CMake feature probes compile at `-O0`. If the ambient glibc defines
  `_FORTIFY_SOURCE`, `-Werror` turns the resulting warning into a failure.

The shell now provides `go` and exports `CFLAGS`/`CXXFLAGS=-O2`. The second
cause also has a cache dimension: a `target/debug/build/aws-lc-sys-*` directory
configured in a different shell is reused in place, so a Go-enabled or
fortify-enabled configure can fail even after the environment is fixed. The
shellHook and `README.md` name the one-line purge; the full workspace was
rebuilt from a clean `aws-lc-sys` cache inside the shell as the gate.

## 2026-10-25 — The rebuild log names its scan floor

When the stored BDK checkpoint is orphaned, the wallet rebuilds and full-scans.
The log said what triggered the rebuild and which heights were involved, but not
where the scan started. On the bitcoind backend that start height is the
substance of the path: it is the minimum of the configured birthday, the
earliest confirmed anchor in the old transaction graph, and the orphaned
checkpoint height. With `birthday=0` the minimum is 0 and the source is
`configured_birthday`, so the rebuild rescans from genesis and the anchor never
wins. That is correct for an explicit 0, but non-obvious; the anchor path only
engages when the birthday is set above the wallet's activity. Before this change
a genesis rescan and a graph-derived floor looked identical in the old line,
which is exactly the ambiguity that made a previous reorg reconstruction depend
on reading raw block headers.

The line now appends `scan_floor=<height>` and
`floor_source=<configured_birthday|graph_anchor|orphaned_height>`.
`floor_source` names the input that actually won, not just the resulting value.
The value expression is unchanged. Ties go to the orphaned cap, then the graph
anchor, then the configured birthday. Esplora and Electrum scan scripts from
derivation index 0 with a stop gap and have no height floor, so their rebuild
line keeps the existing fields without inventing one.

Floor resolution moved to the orphaned-checkpoint branch so the log and the
rebuild use one value: `reset_and_full_scan` now receives the resolved floor
instead of deriving it again. The WARN text and fields are unchanged. Unit tests
pin that a configured birthday above the graph anchor does not win, and that the
fallback, capping, anchor-only, and tie sources are reported correctly. The live
check on devimint regtest runs twice: with `birthday=0`, expecting
`scan_floor=0 floor_source=configured_birthday`; and with `birthday=500000` after
funding a deposit and invalidating a block, expecting `floor_source=graph_anchor`
with a non-zero `scan_floor` plus the WARN.

## 2026-10-24 — Live reorg verification, stack merge, public launch

State at start: four stacked draft PRs open (#3 rebroadcast idempotency, #4 chain-trait
refactor, #5 electrum + chain.json, #6 bitcoind + scan floor). Locally green. Repo private.

Live gates (devimint regtest, v0.12.0):
- PR1: reorg regression test, behaviour-identical; new backend= log field confirmed.
- PR2: electrum parity at 60401; chain.json persistence (0600, versioned envelope,
  canonical() carries creds, describe() redacts).
- PR3: deterministic invalidateblock reorg on the bitcoind backend with NO esplora
  restart. Previously impossible — electrs-esplora 0.4.1 panics at
  src/new_index/schema.rs:298 on a live invalidateblock.
- §41 answered: 10-block reorg over a CREDITED deposit. Deposit orphaned to 0 confs, then
  re-mined at the same height on a different block hash. Ecash balance unchanged
  (148,743,680 msat), row stayed Done. No double credit. Rebuild path exercised twice in
  five minutes and held both times.

Forensics note: the reorg was reconstructed after the fact from getchaintips (an invalid
branch with branchlen 10 forked at 266) plus block headers. getchaintips is the fast way
to establish reorg depth retrospectively.

Merge: stack merged bottom-up with merge commits (#3 → #4 → #6; #5's content reached main
via #4). Merge commits mean no rebasing is needed, because the base's commits become
shared history.

Gotcha — deleting a PR's base branch CLOSES the dependent PR rather than retargeting it.
A closed PR can't be retargeted ("Cannot change the base branch of a closed pull request")
and can't be reopened while its base is missing. Recovery: recreate the base branch at its
old SHA, reopen, retarget to main, delete the base again.

Gotcha — && in multi-command pastes. "gh pr ready 4 && gh pr merge 4" silently skipped

#4's merge when ready failed, and #5 then merged into the wrong base. Use ; or one command
per line.

Gotcha — backtick-only lines are eaten on paste into bash. A heredoc line consisting only
of three backticks loses them and becomes blank, and the following line can lose its first
character (INFO → NFO). Workaround: use 4-space indented code blocks in gh issue
create/edit bodies.

Publication: repo flipped public; MIT confirmed; description + topics set; issues enabled;
wiki and projects off; Actions token read-only; Dependabot alerts and security updates on.

Branch protection on main: required check "check", 0 required approvals,
enforce_admins false, linear history, no force-push, no deletions. Linear history means
future merges must be --squash or --rebase; --merge is now rejected.

Collaborator: m1sterc001guy has write. Cannot push to main directly (PR required), can
self-merge passing PRs. Settings and protection stay admin-only.

Issues filed: #7 chain-id silently falls back to Regtest; #8 init silently defaults the
network; #9 REPL paste mangling and unknown-command handling; #10 noisy background task
shutdown on quit; #11 rebuild log line must print the derived scan floor.

Next: #11 (cheap, makes the scan floor observable), then #7 + #8 as one PR, then lnv2.

## 2026-10-22 — The chain source becomes a trait; Electrum and bitcoind RPC arrive

Esplora was hard-wired into the BDK wallet through a URL field. That was fine
while it was the only option, and wrong as soon as it was not. The chain source
is now one trait, and the trait's shape was chosen by the least flexible
backend.

Esplora and Electrum are script-pubkey query clients: build a request from the
wallet's descriptors, ask the server, apply the response. bitcoind RPC is not.
`bdk_bitcoind_rpc` emits blocks and the wallet applies them one at a time, so
there is no response object to return. Rather than special-case it, `sync` and
`full_scan` take `&mut Wallet` and each backend does whatever its upstream
library supports. The alternatives were a trait Esplora and Electrum could
satisfy while bitcoind sat outside it, or three parallel code paths.

Two rules keep that from becoming a mess:

- The backend never persists and never owns the BDK database. `BdkWallet` stages
  and persists; the backend only touches the `Wallet` it is handed. The reorg
  rebuild has to checkpoint, rename, and reopen SQLite in a specific order, and
  that ordering is the crash-safety argument.
- The reorg anchor check stays above the backend. It compares the stored
  checkpoint with the backend's tip and the hash at that height, using a pure
  function, so all three backends are protected by the same code rather than by
  three copies of it. `MockBackend` tests prove the decision path for every
  backend kind without a network.

What does not fit is restore. Esplora and Electrum discover history with a stop
gap; bitcoind cannot, because a block walk only sees payments to scripts the
wallet already tracks. On bitcoind a full scan needs a birthday height, reveals
a bounded script range, and walks blocks from there. A genesis scan is refused
outside regtest, and the birthday is persisted because a wrong one silently
drops history. That asymmetry is documented rather than papered over.

Configuration grew up with the second backend. The chain source lives in
`chain.json`, a caller can still pass one explicitly, and a mismatch is logged
instead of being applied silently: opening an Electrum wallet against the
default Esplora URL used to be an easy, invisible mistake.

Electrum and bitcoind RPC have unit coverage and no live run yet. devimint's
esplora includes an electrs whose regtest Electrum RPC is on 127.0.0.1:60401,
and the devimint bitcoind is the obvious place to finally run the
`invalidateblock` reorg test that the Esplora binary cannot survive.

## 2026-10-22 — CLI-first MVP, with Lightning and alternate backends in scope

The near-term goal is now a **CLI-first MVP**. A desktop GUI and a mobile app are
deferred until after that MVP. The MVP scope was widened:

- on-chain send/receive;
- Fedimint peg-in/peg-out;
- ecash balance;
- Lightning send/receive through `lnv2`;
- three BDK chain backends: Esplora, Electrum, and bitcoind RPC;
- restore and backup;
- crash safety;
- fee preview at deposit request;
- progress output from `watch`.

This is a scope decision, not an implementation claim. Lightning and the Electrum
and bitcoind RPC backends do **not** exist yet. Esplora is still the only wired
chain source, and `lnv2` is not registered. The MVP checklist in `ROADMAP.md`
records that honestly.

## The starting point

The project began as a Fedimint wallet with a separate BDK on-chain wallet. The
two are deliberately different trust domains:

- The BDK wallet is self-custodial. Its BIP39 seed, BIP84 descriptors, and UTXOs
  belong to the user.
- The Fedimint ecash balance is custodial at a federation while it is pegged.
  The federation can halt, censor, or rug. The wallet cannot make that trustless.

That split drove most of the architecture. A deposit moves BTC from the BDK
wallet to a federation deposit address and then claims it into ecash. A
withdrawal moves ecash out through the federation's on-chain wallet to an
external address. The BDK wallet is not necessarily the withdrawal destination,
so withdrawal reconciliation cannot depend on a BDK address scan.

## Verifying the APIs before building

The first serious work was API verification against the real pinned crates, not
against a plan. That changed several assumptions:

- The current module family is `walletv2` + `mintv2`, not the older wallet module
  API.
- `ClientPreview::join` and `ClientPreview::recover` return a `ClientHandle`, not
  a `Client`.
- `Database::with_prefix` takes a `Vec<u8>`.
- `FeeQuote` is not serializable in the pinned version, so the journal stores a
  small wire form.
- `await_receive` returns the final state and a cursor, not the operation id or
  claim figures; the claim data comes from the operation log.
- `bdk_esplora` has `EsploraAsyncExt::sync` and `full_scan`, not an
  `EsploraClient` with a `scan` method.

Those findings are recorded in `docs/API-VERIFICATION-FIRST-PASS.md` and
`docs/API-VERIFICATION.md`. Later live experiments superseded some early
assumptions, especially around restore and change outputs.

## Keys and network

The wallet uses a 12-word BIP39 mnemonic. Fedimint's root secret follows the
canonical `Bip39RootSecretStrategy` derivation with the `Fedimint Client Salt`;
BDK uses the same seed through BIP84 external/internal descriptors. The keys are
not reused directly, but the mnemonic is common to both wallets, so leaking it
loses both balances.

The network is derived from the federation's block-1 chain id. Mainnet, testnet,
signet-4, and Mutinynet have known mappings; everything unknown maps to Regtest.
That fallback is useful for development but is an operational hazard: it does not
fail closed. The wallet displays the federation id and network, and users must
verify them before funding.

## The journal and crash safety

The journal is the application's crash-safe state store. It is a keyed state
store with atomic transitions, not an append-only event log.

Each row is stored in a serde JSON envelope with a version byte. Rows and indexes
are keyed by one-byte prefixes: deposit, withdrawal, claim ownership, scan
watermark, open-deposit index, open-withdrawal index, pending net-credit index,
and withdrawal-operation index.

The important rules are:

- one open deposit and one open withdrawal per federation;
- one claim owner for each `(federation, outpoint)`;
- one withdrawal row for each walletv2 Send operation;
- the scan watermark advances only after a scan reaches a terminal condition and
  does not exhaust the bounded state-wait budget;
- a deposit is not credited until a receive operation reaches `Success`;
- a `Done` deposit whose mintv2 operation fee is not known keeps
  `net_credit = None` and is indexed for later reconciliation. It never becomes
  a guessed zero.

Those rules exist because the wallet has two sources of truth: a local database
and federation/chain side effects. The database transaction can be atomic; a
Bitcoin broadcast or a Fedimint send cannot be. The code therefore records
idempotency keys and operation ids, binds claims exclusively, and recovers
withdrawals through the operation log's `custom_meta.id` instead of sending
again.

The crash matrix covers address shown before funding, funding broadcast before
claim, crash during confirmation, crash after `Done`, crash between `send()` and
operation-id persistence, and crash after the final send state before
`Broadcast`. It does not prove every narrow window. A crash after the network
broadcast but before BDK persistence/journal recording is still an audit target,
and the change-output metadata has a known verification gap.

## First live round trips and fees

The first live work was on Mutinynet. The wallet joined a federation, generated a
deposit address, funded it from BDK, claimed it into ecash, and withdrew back to
an external Bitcoin address.

The first accounting result was wrong in an instructive way: the displayed net
credit did not match the actual ecash balance delta. The missing piece was the
mintv2 operation fee. The journal now records the gross deposit, the on-chain
claim fee, and the operation fee, and computes net credit from all three. If the
operation fee is unavailable, it stays pending rather than being underestimated.

The live runs also exposed change-output waits and a small quote/drift mismatch
on withdrawals. The change wait is bounded so a timeout leaves the withdrawal
`Submitted` for reconciliation instead of blocking forever. The quote mismatch
(224 msat in one observation) is still unresolved.

## Restore and backup

A fresh mnemonic-only restore recovers settled balances but not the local
operation log. Deposit and withdrawal history is empty by design. More seriously,
0.12.0 does not resume in-flight operations, and there is no public walletv2
rescan/re-claim path. A funded, unclaimed deposit can be stranded at the
federation deposit address. Requesting a new deposit does not recover it.

The recovery artifact is therefore **mnemonic plus the client data directory**,
not the mnemonic alone. The CLI provides full-directory backup and import. The
backup copies the mnemonic, BDK SQLite plus WAL/SHM sidecars, and the RocksDB
directory; it takes a non-blocking fedimint lock and refuses a live database. The
archive is age-encrypted with a separate backup passphrase (see the 2026-10-31
entry above). A journal-only export is not a recovery backup.

## Reorgs and chain truth

The wallet initially assumed BDK handled a reorg. It does not handle one specific
case: when the remote tip is lower than the local checkpoint and the overlapping
hashes agree, `bdk_chain` can return an empty changeset and leave orphaned
anchors confirmed. The wallet now checks the stored checkpoint against the remote
tip before syncing. If the checkpoint is orphaned, it builds a fresh BDK database,
full-scans it, checkpoints and closes both connections, atomically swaps the
scanned file into place, reopens a fresh connection, and preserves the next
keychain index.

The SQLite handoff matters: keeping the old connection after the rename makes the
first write fail with `SQLITE_READONLY_DBMOVED` (1032). The swap/reopen fix is
covered by a regression test. The devimint Esplora server itself panics on a live
`invalidateblock`, so deterministic reorg testing needs a restart of that server.

## Interfaces, operations, and the current state

The core is exposed through a thin UniFFI object and a development CLI. The FFI
has one namespace and one error type, and it does not install logging. The CLI
installs a stderr subscriber, but the REPL is not a product UI. Both use the same
state machines.

The repository also gained:

- a full-directory backup/restore path with live-lock failure;
- a CLI `backup`/`restore-backup` path;
- FFI smoke tests for the generated Kotlin surface;
- strict fmt/clippy/test CI, with a non-blocking advisory check and explicit
  advisory ignores;
- a Nix devShell with Rust 1.91.1, native dependencies, and derived bindgen
  environment;
- public architecture, threat-model, security, audit, and roadmap documents.

The code is still pre-alpha. There has been no independent audit and no recorded
mainnet run. The target now is the CLI-first MVP, not a GUI.

## Open threads

- Lightning/`lnv2` send/receive and gateway requirements are not implemented or
  API-verified in this repository.
- Electrum and bitcoind RPC backends are dependencies but not wired; Esplora is
  the only chain source.
- On-chain arbitrary send/receive is not a complete product flow yet.
- The deposit mintv2 operation fee is not previewed at request time.
- `watch` has no progress output.
- The 224 msat withdrawal quote mismatch and the phantom `change_outpoint_range`
  txids remain open.
- A reorg that orphans an already-credited deposit is not deterministically
  tested.
- Fresh restore can strand in-flight funds; the upstream re-claim gap remains.
- The desktop GUI and mobile app are deferred until after MVP.
