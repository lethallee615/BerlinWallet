# Journal

This is a plain-language record of what Berlin Wallet is, what was built, and why.
It is not API documentation and it is not a changelog of every commit. The source
and `docs/API-VERIFICATION.md` remain authoritative for current behavior.

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
archive is unencrypted by design, so it must be encrypted externally. A
journal-only export is not a recovery backup.

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
