# Threat Model

This is the threat model for the pre-alpha core and its UniFFI boundary. It is
not an audit report, and it does not claim that the listed mitigations are
sufficient. The code is unaudited.

The wallet combines two different trust models:

- **BDK on-chain wallet:** self-custodial. The mnemonic/descriptors control the
  UTXOs; the Esplora server is trusted for chain observations.
- **Fedimint ecash:** custodial by construction. A federation threshold holds the
  bitcoin backing and issues ecash. The federation can halt, censor, or rug.

The application must not blur those two statements. A successful deposit moves
BTC from the self-custodial BDK wallet into the federation's custody. A
withdrawal moves it back out, subject to the federation's send and the chain.

## 1. Assets

- The BIP39 mnemonic and the derived BDK private keys.
- The BDK UTXOs.
- The Fedimint ecash notes/balance and the federation's backing.
- The client data directory: mnemonic, `bdk.sqlite`, `fedimint.rocksdb`, the
  journal, and any backup copy.
- Private metadata: addresses, amounts, timing, federation id, client identity,
  and the fact that a given wallet uses a given Esplora/federation endpoint.
- Availability of the wallet and the ability to withdraw.

## 2. Trusted and untrusted parties

| Party | Trust assumption | What it can see |
|---|---|---|
| User | Trusted to keep the device, OS, mnemonic, and backups safe. | Everything. |
| Local OS / LUKS | Trusted while running; LUKS is assumed to protect the disk only while locked. | The plaintext data directory and process memory once unlocked. |
| BDK and the Rust dependency graph | Trusted code execution. No independent verification of signing or chain logic. | Whatever the process can see. |
| Esplora server | Trusted for chain height, block hashes, transactions, UTXOs, and broadcast. | Every address/script the BDK wallet reveals, plus IP/timing; can link the wallet's on-chain history. |
| Fedimint federation | Trusted as a threshold custodian for the peg. | Deposit/withdrawal addresses, amounts, timing, and the ecash operations it processes. |
| Network / TLS terminator | Not trusted to be honest; may observe or alter traffic if the endpoint is not authenticated. | Esplora and federation traffic metadata/content depending on transport. |
| FFI host / GUI | Trusted application process for the wallet instance. It owns logging and the data-directory path. | The FFI calls and returned records. |

The CLI's `DEFAULT_ESPLORA_URL` is `http://127.0.0.1:3002`
(`crates/berlin-wallet-cli/src/main.rs`). For anything other than a local dev
chain, use an authenticated HTTPS endpoint (or another authenticated transport)
and understand that the endpoint still sees the addresses it serves.

## 3. What breaks if each party lies

### Federation

The federation is **custodial by construction**. A threshold of guardians can:

- halt deposits/withdrawals or refuse to process a claim;
- refuse to sign a withdrawal or freeze the user's ecash;
- misreport walletv2 receive/send final states;
- move or lose the federation's on-chain backing;
- rug the peg entirely.

The wallet's only check on a deposit credit is the walletv2 operation log and
the final receive state reported by the federation; the claim is local, but the
authoritative peg state is theirs. The wallet cannot independently prove that
the federation's on-chain backing is solvent. The threshold matters, but it is
not a trustless bridge.

What the federation cannot do without the mnemonic is sign a transaction from
the user's BDK wallet. It can, however, observe and correlate the on-chain
funding transaction and the deposit address.

If the federation claims a send succeeded but the txid is not on chain, the app
records the federation's `Success(txid)`. There is no independent confirmation
policy for that withdrawal in the current state machine; the chain is what users
must ultimately check.

### Chain source (Esplora, Electrum, or bitcoind RPC)

The wallet uses exactly one BDK chain source, selected by configuration and
recorded in `chain.json`. Whichever is chosen:

- sees every address/script the BDK wallet reveals;
- can link all of those addresses to each other and to the user's IP/timing;
- sees the deposit funding transactions and any withdrawals sent to addresses
  it can observe;
- can lie about the chain tip, block hashes, transactions, UTXOs, and
  confirmations;
- can censor a broadcast or serve a stale/forked view.

The trust boundary differs by backend:

- **Esplora** — an HTTP server, usually someone else's. Full address history
  visible to that operator.
- **Electrum** — a socket server, usually someone else's. Script-hash queries
  reveal the wallet's whole script set over time (a script-hash query is
  trivially linked back to an address by the server if it has the chain).
- **bitcoind RPC** — your own node, or a node you are trusting with a wallet's
  query pattern. No third party sees the script set, but the node can still lie,
  and the RPC credentials live in `chain.json` at mode 0600 alongside the
  plaintext mnemonic.

A single chain source creates a single linkability point: using one endpoint for
the wallet's whole history lets that operator correlate the wallet's addresses —
unless you operate the node yourself, in which case the linkability point is the
node operator, who is you. The code does not use independent chain sources or
privacy-preserving queries, and switching backends does not unlink past
history.

`BdkWallet::sync` verifies ancestry and rebuilds/full-scans when the stored
checkpoint is orphaned. That is a correctness guard against the BDK
height-decreasing-reorg behavior; it is **not** a Byzantine-chain defense. A
server that consistently lies can still feed the scan and the ancestry checks.

### BDK and the Rust dependency graph

BDK is trusted to derive the same descriptors, build/sign valid transactions,
persist the wallet correctly, and apply chain updates. A bug or a compromised
dependency can:

- derive/reveal the wrong address;
- sign or broadcast an unintended transaction;
- lose or corrupt the BDK database;
- accept a wrong checkpoint;
- expose key material through memory, logs, or a dependency.

The workspace pins exact versions and uses a Git tag for Fedimint, but there is
no independent audit of BDK, Fedimint, or the transitive graph. The size of the
graph is a supply-chain risk. `deny.toml` records the currently accepted
licenses and advisory ignores; it does not prove the code is safe.

### OS / LUKS boundary

LUKS/full-disk encryption protects the data directory at rest only while the
volume is locked. Once the OS is running and the volume is unlocked:

- the mnemonic file is readable by the user and by any process that can read the
  data directory;
- the wallet process has the BIP39 seed and signing keys in memory;
- the federation client has its root secret and database open;
- malware, a compromised host, or a malicious FFI caller can read or manipulate
  all of it.

There is no hardware wallet, secure enclave, memory zeroization guarantee, or
anti-keylogging layer. A locked screen is not a security boundary against a
compromised OS.

### The mnemonic in plaintext at rest

`<data-dir>/mnemonic` is plaintext, mode `0600` on Unix. Anyone who can read it
can derive both the BDK keys and the Fedimint root secret. This includes:

- another local user/process with read access (including root);
- disk snapshots/backups;
- a stolen unlocked or decrypted disk;
- a data-directory backup copied to another machine.

The mnemonic is the common root for both wallets; compromising it compromises
both balances. Encryption at rest is a roadmap item, not a current property.

### Unencrypted backups

`backup_data_dir` copies the **whole data directory**, including the mnemonic,
`bdk.sqlite` plus WAL/SHM sidecars, and the RocksDB directory. The archive is not
encrypted by the application. It:

- is as sensitive as the mnemonic;
- can be read by anyone who obtains it;
- may be stale or inconsistent if the operator ignores the "stop the wallet"
  rule;
- passes the fedimint lock check but does not take a global BDK/RocksDB snapshot
  across both databases.

Encrypt backups externally (`age`, `gpg`, an encrypted volume) until an
authenticated, versioned encrypted archive exists. A backup failure can lose
history or in-flight operations; a restored full directory is required to
preserve the client operation log and scanner state.

### Single-open data directory

There is no application-level cross-process lock for the whole data directory.
RocksDB and SQLite have their own locks, but the BDK SQLite file and the
Fedimint DB are separate. Running two processes against one directory is
unsupported and can corrupt state or produce conflicting wallet actions.

`backup_data_dir` takes a non-blocking lock on the Fedimint DB's
`fedimint.db.lock`, so it refuses a live fedimint DB. That does not make it safe
to run two wallet instances: the operator must still stop the wallet and ensure
that only one process owns the directory. The FFI `shutdown()` contract is part
of this boundary: mobile must call it before backgrounding so the DB lock is
released.

### Invite code, federation id, and network

The invite code selects the federation. The wallet displays the federation id
and client identity, but the current CLI/FFI do not have a full trust-on-first-use
or federation-verification UX. A user who joins the wrong federation sends funds
to the wrong custodian.

`network_from_chain_id` maps unknown chain IDs to `Network::Regtest` rather than
failing closed. This prevents an unexpected chain from producing a clear error,
and it can cause addresses/builds to be made for the wrong network. Verify the
federation and chain id before funding.

### Host application and FFI boundary

The FFI is a thin boundary, not a separate security sandbox. The host app can
read the data directory, call every method, and see returned records. The FFI
flattens core errors into `PegWalletError::Message { message }`, so the host
cannot reliably branch on error classes. The FFI does not install a tracing
subscriber; logs are the host's responsibility. A malicious or compromised host
is outside the wallet core's defense.

## 4. What is explicitly out of scope

- A malicious host application, malicious GUI, or compromised OS process while
  the wallet is running.
- Physical attacks, cold-boot attacks, memory scraping, fault injection, and
  side channels.
- Network anonymity or chain-analysis resistance. Esplora and the federation can
  see and correlate addresses; there is no Tor/I2P or blinded chain-source
  design.
- Federation governance, guardian compromise thresholds, and the legal or
  operational solvency of a federation.
- Hardware wallets, multisig on the BDK side, secure elements, and password-based
  seed encryption.
- Mainnet operational security, monitoring, incident response, and exchange/
  custody integration.
- Recovery from a compromised device after the seed has been exposed.
- Multi-federation management and cross-federation privacy.
- Encryption of backups by this application (the operator must do it).
- Security of third-party Esplora, Fedimint, BDK, Rust, or platform code beyond
  the pinned versions and the tests in this repo.
- Regulatory, tax, accounting, and legal compliance.

## 5. Security-relevant limitations in the current code

The verification record owns the details; this section only links to it so the
two cannot drift.

- [§30/§31: fresh restore, in-flight operations, and backup policy](API-VERIFICATION.md#30-in-flight-operations-across-a-fresh-restore-live-experiment) ([§31](API-VERIFICATION.md#31-recovery-policy-and-app-level-backup-0120)).
- [§41: reorg of an already-credited deposit is untested](API-VERIFICATION.md#41-known-limitation--restore-strands-in-flight-operations).
- [§37: walletv2 change outpoints are unverifiable](API-VERIFICATION.md#37-walletv2-operation-meta--change_outpoint_range-is-unverifiable).
- [§38: withdrawal quote and charged fee mismatch](API-VERIFICATION.md#38-withdrawal-quote-vs-charged-fee-mismatch-224-msat).
- [§39: silent `watch` polling](API-VERIFICATION.md#39-observability-gaps).
- [§39: mintv2 operation fee is not previewed at deposit request](API-VERIFICATION.md#39-observability-gaps).
- [§36: devimint Esplora panics on a live reorg](API-VERIFICATION.md#36-devimints-esplora-panics-on-a-live-reorg).

Lightning/`lnv2` is in the MVP scope but is not implemented yet; its gateway,
liquidity, and routing trust model has not been analyzed.

The dependency advisory-check policy is separate from these code findings and is
documented in [`SECURITY.md`](../SECURITY.md#dependency-advisory-policy).

## 6. Review questions

A security reviewer should be able to answer:

1. Can a deposit outpoint be credited to two rows after a crash, reorg, or
   adoption pass?
2. Can the watermark skip an operation whose final state becomes visible later?
3. Can a concurrent `claim_bind`/adoption race create or credit two owners?
4. Can a withdrawal be sent twice after a crash between `send()` and operation-id
   persistence?
5. Can the withdrawal row reach `Broadcast` before the primary change outputs
   are credited?
6. Can two processes open the data directory and race the journal, BDK wallet, or
   RocksDB?
7. Can any error path mark a deposit `Done` without a successful receive final
   state?
8. Can a backup silently omit WAL/SHM or a RocksDB sidecar, or be taken while
   the federation DB is live?
9. Can an unknown chain id or wrong invite produce a funded wrong-network
   address?
10. What is the recovery story after a fresh restore, and which in-flight funds
    are not recoverable?
