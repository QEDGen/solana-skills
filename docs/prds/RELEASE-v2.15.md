# Release v2.15.0

Closes 7 GH issues from `tanmay4l`'s May-1 burst plus folds the v2.15
external Quasar audit eval (internal + blueshift-gg/quasar examples)
into the auditor catalog. Spec_hash sealing tightened end-to-end;
probe runtime detection now correctly classifies hand-written Quasar
versus qedgen codegen output and discovers handlers in both shapes.

## Issues closed

- **#25** — drift checker walks `src/guards.rs`. Spec changes that
  re-emit runtime guard logic now invalidate the file's spec-hash.
- **#26** — Aristotle archive extraction refuses path-traversal
  entries. Validates every entry's stripped path against
  `is_safe_relative_path` (only `Component::Normal` and `CurDir`).
- **#27** — `qedgen check --drift --update-hashes` now refreshes
  `hash`, `spec_hash`, and `accounts_hash` in one pass. Path
  resolution walks parent directories from the source file.
- **#28** — `--deep` mode no longer false-positives on every function
  with non-trivial body. New algorithm: flag transitive drift only
  when a `#[qed(verified)]` callee has itself drifted directly.
- **#29** — partial accounts metadata fail-fast at the macro. Either
  all three of `accounts`, `accounts_file`, `accounts_hash` must be
  provided or none.
- **#30** — escrow `exchange` and `cancel` now bind `initializer_ta`
  to the stored receiver TA via a `requires` clause. The
  `stored_field_never_written` predicate (new in this release)
  catches this class structurally going forward.
- **#31** — spec_hash now folds in a digest of all top-level non-
  handler items in the spec source. Changes to consts, types,
  imports, interfaces, pdas, events, errors, environments,
  properties, invariants invalidate every handler's spec_hash.

## Probe enhancements (eval cycle)

- **D1**: `Runtime::QedgenCodegen` returns the v2.13 Quasar-specific
  catalog plus universal handler-body shapes instead of an empty
  list. Single-line bug; three of four external audits flagged it
  independently.
- **D2**: `Runtime::Quasar` variant added for hand-written Quasar
  code (`quasar-lang` dep with no qedgen markers). Runtime detection
  splits on the `formal_verification/` / `qed.toml` /
  `#[qed(verified)]` triple and prioritizes qedgen markers over
  `Anchor.toml`.
- **D3**: handler discoverer for Quasar's
  `#[program] mod X { #[instruction(discriminator = N)] pub fn h(...) }`
  shape. Falls out of the Anchor parser for free.
- **G1**: `stored_field_never_written` predicate. Spec-aware lint
  that flags state fields read by `auth <field>`, `requires`,
  effect RHS, or properties but never written by any handler
  `effect`. PDA-seed fields are suppressed.
- **G2**: `invariant_no_body` lint. `invariant <name> "<doc-string>"`
  without an `expr` body errors at `qedgen check` time so codegen
  doesn't silently emit `theorem <name> : True := trivial`.

## Auditor catalog

- 18 new probe categories across token / escrow / lamport-vault /
  multi-actor-quorum / probe-meta families.
- New multi-actor / quorum primitive family (5 categories) — was
  missing entirely from the v2.14 catalog. Plus an escalation rule:
  multisig / governance / committee programs without on-chain
  proposal-objects are category-zero findings regardless of catalog
  hits.
- 14 new cookbook chains, severity rubric `implicit-runtime-invariant`
  tag, cross-runtime sibling-diff methodology, runtime detection
  guardrails.
- 9 new exploits.md entries (multisig dup-signer, multisig nonce-
  absent replay, authority-transfer race generalization, privileged-
  role-outside-quorum, vault-PDA owner-drift / Loopscale-shape, vault
  rent-floor unenforced, implicit-mint-check reliance, idempotent-init
  silent no-op, upstream interface-forgery on stored program-id) plus
  a four-axis upstream-pin detection sequence.

SKILL.md grew from 819 to 1170 lines. exploits.md from 1032 to 1212.

## Latent fixes folded in

- `accounts_struct_hash` byte-mirror divergence between qedgen-side
  and macro-side. Both sides now use `canonical_token_string`
  (matching the v2.11.3 fix for body_hash).
- Codegen lowering for `<acct>.pubkey` in `requires` clauses. Pre-
  v2.15 the codegen emitted the literal `acct.pubkey` which doesn't
  resolve in the generated Rust; now lowers to
  `(*ctx.<acct>.to_account_view().address())`.
- Lean codegen drops `requires` clauses that reference handler-
  account pubkeys (handler accounts have no Lean scope). The runtime-
  side check still emits in Rust; only the Lean projection is dropped.

## Migration

- Any user with committed `#[qed(verified, spec_hash = "...",
  accounts_hash = "...")]` attributes will see drift on first build
  under v2.15 because both hash algorithms have shifted (#31 + the
  byte-mirror fix). Run `qedgen check --spec <path> --drift <code>
  --update-hashes` to refresh; the new flow handles all three legs.
- Examples bundled in this repo (escrow, escrow-split, lending,
  multisig, percolator) regenerated against v2.15 with refreshed
  spec_hashes and the v2.15 codegen output.

## Gates

- 482 unit + 24 macro tests pass.
- `cargo fmt --check` clean. `cargo clippy -- -D warnings` clean.
- `bash scripts/check-readme-drift.sh` — 17/17 commands documented.
- `bash scripts/check-version-consistency.sh` — 2.15.0 across crate
  + npm package.
- `bash scripts/check-lake-build.sh` — 10/10 examples build.
- `qedgen check --regen-drift` — 5/5 examples drift-clean.
- `qedgen check --frozen` against bundled spec dirs — clean.
- Zero unauthorized `sorry` (only the v2.8 G3 ensures-as-axiom
  theorems remain, by design).

## Eval artifacts

The v2.15 audit eval against internal (`escrow`, `escrow-split`,
`lending`, `percolator`) and external (blueshift-gg/quasar `escrow`,
`multisig`, `vault`, `upstream-vault`) examples is preserved under
`.qed/findings/` for internal traceability:
- `phase1-rollup.md` — internal eval: 8 prioritized gap categories
- `phase2-rollup.md` — external eval: 19 new probe categories, 14
  cookbook chains, 10 exploits entries; 4 detector bugs (D1-D5)
- `gh-issues-crossref.md` — mapping from each open GH issue to eval
  overlap and v2.15 close path
- per-target findings files (4 internal + 4 external)

Per the no-naming policy (`feedback_no_anchor_v2_mentions`), the
SKILL.md / exploits.md / RELEASE-notes text uses generic taxonomy
throughout. External-target attribution is confined to internal
`.qed/findings/` files.
