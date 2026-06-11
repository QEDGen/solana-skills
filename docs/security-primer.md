# Solana Security Primer

Long-form reference for the exploit classes and operational
threat-model shapes the QEDGen Auditor's category catalog cites
in its `Corpus:` lines. This file lives outside the loaded skill
surface — it's intended as human-readable background for someone
exploring the repo, **not** loaded into the auditor's context on
every invocation. The auditor's working memory stays on
`.claude/skills/qedgen-auditor/SKILL.md`; this primer is what you
read when you want the *story* behind a Corpus reference.

If you're writing or extending an audit catalog rule, the loop is:
read the loss / attack flow here, generalize the shape, and add it
to a category's `Spec-less per-runtime` predicate + `Corpus:` line
in `SKILL.md`. Don't expand this primer at the cost of catalog
expressiveness — the primer is reference, the catalog is the
working surface.

Incidents are described by their *vulnerability class*, not by the
protocol they happened to. The shape is what generalizes; the name
is not.

---

## Part 1 — On-chain exploit classes

The nine classes below are public, code-pattern exploits (not
key-management / supply-chain incidents — those are in Part 2).
Each maps to one or more SKILL.md categories' `Corpus:` line.
Auditors investigating that category should re-read the relevant
entry here to refresh their mental model of *how the primitive
actually got exercised at scale*.

### Sysvar-instructions spoof (2022, ~$326M)

**SKILL.md categories:** `account_type_confusion`, `arbitrary_cpi`

**Root cause:** signature verification used a deprecated
instruction-introspection call that did not check that the input
account was the real `sysvar::instructions` account; attacker
passed a fake sysvar that looked like a successful prior secp256k1
verification.

**Attack flow:**
1. Construct an account whose data mimics the layout the
   instruction-introspection sysvar produces after a successful
   Ed25519 / secp256k1 verify.
2. Pass that fake account in the position the program expected
   the real sysvar.
3. The verifier reads the fake "previous" verification, marks the
   guardian / signer set as approved.
4. Call the privileged mint path to issue wrapped assets with no
   collateral-side lock.

**Grep for:**
- `load_instruction_at(` (deprecated; use
  `load_instruction_at_checked`)
- Any read of `Sysvar::Instructions` data without
  `solana_program::sysvar::instructions::ID` equality check
- Anchor: `AccountInfo<'info>` for the instructions sysvar
  instead of `Sysvar<'info, Instructions>`
- More generally: any "well-known" account (token program, system
  program, rent, clock) typed as `AccountInfo` rather than its
  strongly-typed wrapper

**Composes with:** missing program-id check on CPI target;
account type confusion (treating any AccountInfo as a sysvar).

---

### Fake-account collateral chain (2022, ~$52.8M)

**SKILL.md categories:** `field_chain_missing_root_anchor`,
`missing_owner_check`

**Root cause:** no "trusted root" — the `mint` field on the leaf
account was never validated against the real LP mint. Attacker
forged the entire chain (leaf → collateral-tokens → mint) with
worthless underlying.

**Attack flow:**
1. Create a fake leaf account with a mint pointer to
   attacker-controlled tokens.
2. Create a fake collateral-tokens account that points to the
   fake leaf.
3. Pass the fake chain into the mint instruction, which trusts the
   leaf and walks up.
4. Mint billions of stablecoin units against zero real collateral;
   dump.

**Grep for:**
- Account-A reads pubkey field from Account-B without verifying
  B's owner *and* B's contents pin back to a known-good root
- Multi-account validation chains where each link is checked
  locally but no link is anchored to a hardcoded program-id, mint,
  or PDA derivation
- Any `account.data.field` used as authority or routing key
  without `account.owner == &expected::ID`

**Composes with:** missing owner check (forgery cheap); type
confusion (forgery undetectable); arbitrary CPI (forgery directs
the swap path).

---

### Thin-spot-oracle cross-margin manipulation (2022, ~$114M)

**SKILL.md categories:** `oracle_staleness`,
`frontrunnable_no_slippage`, `flash_loan_amplified_governance`

**Root cause:** thin spot oracle for a low-liquidity governance
token; single-block price ramp via cross-trading from one attacker
account to another, leveraging the artificially-inflated token as
collateral against real assets.

**Attack flow:**
1. Open offsetting long+short perp positions in the thin token
   from two attacker accounts (~$10M seed).
2. Pump the token's spot price ~45x in ~10 minutes with
   low-liquidity buys.
3. Long-side account's collateral value balloons; borrow ~$114M
   of real assets against it.
4. Walk away — the short side gets liquidated, but the borrows
   already left.

**Grep for:**
- Oracle reads with no TWAP / no min-confidence-interval check
- Single-source price feeds (one oracle product, no cross-check)
- Collateral valuation that uses spot price without
  liquidity-adjusted haircut on thin markets
- `borrow_value = collateral_value * ltv` patterns where
  `collateral_value` is a single oracle read

**Composes with:** thin spot market = manipulable input;
cross-margin pools = single-account-can-drain-all; no per-asset
borrow caps = no circuit breaker.

---

### Fake CLMM tick account (2022, ~$8.8M)

**SKILL.md categories:** `account_type_confusion`,
`missing_owner_check`

**Root cause:** a concentrated-liquidity AMM accepted an
attacker-supplied "tick" account without owner check or PDA
derivation check; the tick stored in-band fee-growth values that
the program trusted.

**Attack flow:**
1. Flash-loan from a lending protocol.
2. Create a fake `tick_array` account with crafted
   `fee_growth_outside` values.
3. Open and immediately close a position that "earns" the crafted
   fees on swap fees.
4. Collect inflated fees, repay flash loan, profit.

**Grep for:**
- Tick / order / position accounts passed via `AccountInfo`
  without `seeds = [b"tick", pool, tick_index]` Anchor
  constraint
- Manual PDA derivation that compares only one of (program_id,
  owner, address) — must check all three
- Accounts that store accounting state (fee growth, cumulative
  index) with no owner check

**Composes with:** flash-loan amplifier (attacker doesn't need
capital); CLMM in-band accounting (the lie compounds with each
tick crossed).

---

### Stable-swap bidirectional rounding (2022, ~$700M at risk; public pre-exploit disclosure)

**SKILL.md category:** `rounding_direction_round_trip`

**Root cause:** stable-swap rounding rounded *toward* the LP, then
*also* toward the LP on the reverse swap; attacker round-trips
draining one token per instruction.

**Attack flow:**
1. Swap A→B with input that triggers round-up favoring user.
2. Swap B→A with input that triggers round-up favoring user.
3. Net: one extra token per round-trip, fee-free at Solana's flat
   5000-lamport tx cost.
4. Pack 100s of swap instructions per tx; drain pool over hours.

**Grep for:**
- Bidirectional pair functions (deposit/withdraw, mint/redeem,
  swap A↔B) using the *same* rounding direction on both sides
- `try_round_u64`, `.round()`, `ceil_div` used without asymmetric
  direction (favor protocol on intake, user on payout — never
  both same)
- `(a / c) * b` pattern (divide-then-multiply loses precision);
  should be `(a * b) / c` with overflow check

**Composes with:** Solana's flat-fee model (off-by-one is
profitable, unlike Ethereum's gas market); deep liquidity (many
round-trips before pool empties).

---

### DEX-pool-sourced oracle write-lock manipulation (2022, ~$1.26M)

**SKILL.md category:** `oracle_staleness`

**Root cause:** a stablecoin price feed sourced from a single DEX
pool; attacker write-locked the pool account in the same slot as
the oracle update to prevent arbitrage and inflate the read.

**Attack flow:**
1. Predict the slot the oracle will sample the price.
2. Send a tx in that slot that write-locks the source pool while
   pumping price.
3. Oracle reads inflated price ($1 → $15).
4. Deposit the stablecoin as collateral, borrow ~$1.26M of real
   assets, walk.

**Grep for:**
- Oracle wrapper reading from a DEX pool reserve as the price
  source
- No min-liquidity threshold before accepting an oracle read
- No "max move per slot" rate limiter on collateral revaluation
- Any oracle that reads from an account the user can write-lock
  in the same tx

**Composes with:** single-source oracle (no cross-check);
per-slot transaction ordering (write-lock blocks arbs);
permissionless borrow against single-asset.

---

### Self-priced-token flash-loan pump (2022, ~$3.5M)

**SKILL.md category:** `oracle_staleness` (specifically, the
self-priced-token sub-shape)

**Root cause:** the protocol token's price oracle read from an
internal bonding curve which itself responded to flash-loan-sized
buys.

**Attack flow:**
1. Borrow ~$10M flash loan from a lending protocol.
2. Mint the protocol token via its bonding curve, pumping its
   internal price.
3. Use the inflated holdings as collateral to drain the treasury
   at the new price.
4. Repay flash loan, keep delta.

**Grep for:**
- Treasury / redemption logic that prices its own token via its
  own AMM / bonding curve in the same tx
- Mint and redeem in the same instruction without TWAP between
- "Use my mint price as my collateral price" loop

**Composes with:** flash-loan availability (zero capital
requirement); same-tx price+borrow (no cooldown).

---

### Niche-collateral single-read mispricing (2025, ~$5.8M)

**SKILL.md category:** `oracle_staleness`

**Root cause:** an isolated lending market priced a niche
principal-token from a single point-in-time pool read with no
TWAP; attacker manipulated the source pool to overstate
collateral.

**Attack flow:**
1. Identify which market uses the manipulable principal-token pool
   as its price source.
2. Trade against that pool to push the spot price upward in one
   block.
3. Deposit the principal tokens at inflated valuation, borrow
   stablecoin / SOL up to the inflated cap.
4. Walk; the rest of the market is collateralized in real assets.

**Grep for:**
- New / niche / illiquid token added as collateral with the same
  oracle pattern as blue-chip
- Per-market collateral pricing where the function pulls a fresh
  read each call (no TWAP, no oracle confidence band)
- "Genesis vault" or "isolated pool" that shares a price oracle
  with an attacker-influenceable venue

**Composes with:** young protocol (small liquidity buffer);
permissionless market-add (governance can list a manipulable
token); single-block price + borrow.

---

### Sentinel-null close-account solvency bypass (2022, ~$25M near-miss)

**SKILL.md categories:** `sentinel_null_key_array_short_circuit`,
`pda_lifecycle_reuse_after_close`

**Root cause:** position-array iteration broke the loop when it
encountered `Pubkey::default()` as a sentinel for "closed
account"; attacker placed real positions *after* the closed slot
so they were skipped during the collateralization check.

**Attack flow:**
1. Open a benign position to fill array slot 0.
2. Close it — slot now reads `Pubkey::default()`.
3. Open a real liability position in slot 1+.
4. Borrow against zero counted liabilities (loop short-circuits
   at slot 0).

**Grep for:**
- `for position in positions { if position.key ==
  Pubkey::default() { break; } }` patterns
- Sentinel-terminated arrays where account closure inserts the
  sentinel mid-array
- Any "skip on null-pubkey" loop in a solvency / collateral check

**Composes with:** account-close primitive (creates the sentinel);
permissionless position-open (creates the array hole).

---

## Part 2 — Operational / off-chain threat-model incidents

The five shapes below are *off-chain* — key custody, social
engineering, supply chain, telemetry. They don't map to a code
pattern in SKILL.md because the bug isn't in the program; the
bug is in the operational envelope around it. Auditors should
recognize the shapes because programs are often shipped with
threat-models that handwave these — "the admin key is secure,"
"the dependency is pinned," "the telemetry redacts secrets" —
and a real audit interrogates those claims rather than accepting
them.

### Hot-wallet admin-key trojan (2022, ~$4.4M)

**Threat-model shape:** privileged operations gated by a single
hot-wallet authority; key exfiltrated by malware on the operator's
machine. Operational, not a code bug — but the *program-design
decision* to allow a single Ed25519 signer (not a multisig PDA)
for fee-draw operations is what made the operational compromise
catastrophic instead of recoverable.

**Auditor question:** for every privileged ix (`set_authority`,
`withdraw_admin`, `claim_protocol_fees`), what's the recovery
plan if the signing key leaks? A multisig PDA or time-locked
authority transforms the worst case from "drain in one tx" to
"contestable over a delay window."

**Compose with:** custodial supply-chain risk (npm, dev machine);
no time-lock on admin actions; no rate-limit on protocol-fee
draws.

---

### Self-trade economic-loop exploit (2023, ~$1M)

**Threat-model shape:** margin / PnL accounting that updates from
same-tx self-trade fills — the program had no notion that a trade
could be against itself, so the rebate / margin logic credited
"profits" that were just shuffled capital.

**Auditor question:** for every accounting field updated by
trade fills, is there a "is this trade against yourself" check?
Self-cross / wash-trade should either be banned at the instruction
level or accounted for at the margin level. Defaulting to "trust
the fill" is the bug.

**Compose with:** flash loan; permissionless market-make;
self-cross allowed.

---

### Durable-nonce admin-takeover (2026-04, ~$285M)

**Threat-model shape:** a governance multisig signed
durable-nonce-anchored transactions that were submitted weeks
later. Attackers spent months building social trust as a fake
quant firm, captured signatures intended for one purpose, then
replayed them with attacker-controlled durable-nonce accounts to
execute an admin transfer.

**Attack flow:**
1. Social-engineer access; create 4 durable-nonce accounts (2
   from real council members, 2 from attacker).
2. Capture council signatures on a benign-looking governance op
   anchored to attacker's nonce.
3. Wait. The transactions don't expire because the nonce doesn't
   expire.
4. Submit when context shifts (after a real test withdrawal);
   transfer admin authority to attacker.
5. Whitelist a fake collateral token, deposit 500M of it, borrow
   ~$285M.

**Auditor question:** does the program's admin-transfer flow
have a time-lock? Does it use a fresh recent blockhash (not a
durable nonce)? Is there a nominate→accept handshake (see
`authority_transfer_missing_nominate_accept` in SKILL.md)? Any
one of these would have given the council a window to revoke.

**Compose with:** social engineering (signatures captured under
one pretext, replayed under another); durable-nonce + Solana's
signature-validity model = arbitrary delay; instant admin reconfig
(no time-lock buffer).

---

### Telemetry-SDK key leak (2022, ~$6M, ~9,231 wallets)

**Threat-model shape:** a mobile wallet pointed a crash-telemetry
SDK at a self-hosted endpoint and transmitted private-key material
in error logs. Attacker observed leaked secrets at the telemetry
endpoint and reconstructed private keys for affected users.

**Auditor question:** for any companion SDK shipping with the
program (frontend, signing service, indexer), does any
secret-bearing object pass through a logging / telemetry SDK
without redaction? Any crash-reporting / observability SDK needs
an explicit `beforeSend` scrubber or a structured "this field
never gets logged" pattern.

**Compose with:** off-chain — listed here because Solana program
audits frequently include a companion SDK whose telemetry leaks
PDA seeds, derivation paths, or unsigned tx payloads.

---

### Client-library supply-chain backdoor (2024-12, ~$130-160k)

**Threat-model shape:** spear-phish on an npm publish-credential
holder; two patch versions of a widely-used Solana client library
shipped with code exfiltrating private keys via fake CloudFlare
headers.

**Attack flow:**
1. Phish credentials + 2FA from a maintainer.
2. Publish a backdoored patch version.
3. Auto-upgraders pull the new version into bots, dApps, and
   key-handling services.
4. Backdoor exfiltrates raw keys handled in-process.

**Auditor question:** does the program's deploy pipeline pin
client-library versions with integrity hashes? Does CI run
`npm install` against floating versions before any audit gate?
Any bot / signer holding raw `Keypair` material at
version-resolution time is at risk on every dependency upgrade.

**Compose with:** raw-key custody (anything routing trades or
cosigning); npm dependency floats; no SBOM / pinning.

---

## Part 3 — Out-of-scope reference

**Wallet SDKs and end-user signing flows** are outside the
QEDGen Auditor's scope by design. The auditor analyzes Solana
program code (Anchor, native, sBPF, qedgen-codegen) — the
on-chain attack surface. Wallet UIs, browser extension
permission models, and signing-flow UX live in a different
threat model: they need pen-testing on the signing-prompt
surface, anti-phishing review of the dApp-pairing handshake,
and OS-level isolation review for key storage.

The telemetry-leak and supply-chain incidents in Part 2
illustrate why this boundary matters: both were catastrophic for
users despite the on-chain programs being correct. An auditor
finding "the program holds no admin keys" is a *true* statement
that doesn't bound the user's risk if their wallet leaks
signatures.

When auditing a project that ships both program code and a
wallet / signing companion, escalate the wallet surface to a
dedicated wallet-security review rather than asserting the
program audit covers it.
