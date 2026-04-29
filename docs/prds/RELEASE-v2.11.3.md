# QEDGen v2.11.3 — Lean-side hotfix + drift-hash alignment

**Date:** 2026-04-29
**Type:** Patch release
**Predecessor:** v2.11.2 (Harness loop closure + lint refinements)

## Headline

A v2.11.2 follow-up question — *"why does multisig have no Lean
proofs?"* — uncovered four latent codegen bugs in `lean_gen.rs`, a
silent verification-soundness gap in `init.rs` (every scaffolded
project's `Proofs.lean` was excluded from `lake build`), and a
hash-algorithm divergence between `drift.rs` and the proc-macro that
broke `qedgen check --update-hashes` end-to-end. v2.11.3 ships the
fixes, regenerates affected examples, locks the alignment as a
regression test, and adds the missing pre-release gate
(`scripts/check-lake-build.sh --strict`) that would have caught most
of this.

**All 10 bundled examples (5 Rust + 5 sBPF) now `lake build` clean —
the first release where this is true.** v2.11.2 shipped with escrow
and multisig red on the Lean side.

## What changed (user-facing)

### Lean codegen (four interlocking fixes)

| Bug | Symptom | Fix |
|---|---|---|
| **A** — auth-var as State field | `auth approver` rendered as `signer = s.approver` even when `approver` isn't a State field. Worked only by name coincidence (escrow's `initializer` / `taker`, percolator's `authority`); broke multisig's `approver` / `rejecter`. | New `auth_who_is_state_field` helper. State-field case keeps the guard; alias-only case emits `let <who> := signer` so user-written predicates resolve. Both render paths (`render_transitions` + `render_indexed_state`) share the helper. |
| **B** — account-binding `.pubkey` in effect RHS | `field := initializer_ta.pubkey` in qedspec passed through to Lean unchanged. `initializer_ta` is an account binding with no Lean scope; `Unknown identifier` at the assignment site. | New `is_account_binding_pubkey_ref` helper drops these effects from Lean (Rust side keeps `ctx.accounts.<binding>.key()`). Skipped at three sites: `render_transitions`, `render_indexed_state`, and `WitnessState::apply` (cover-trace simulator). |
| **C** — Map indices as raw `Nat` | `Map[N] T` is `Fin n → α` in Lean. `U8`/`Nat` index params used as `<map>[<param>]` produced `Application type mismatch: ℕ vs Fin N`. | New `infer_idx_promotions` scans handler effects + requires for `<map_root>[<param>]` patterns, promotes scalar params to `Fin <bound>` in transition signatures and `Operation` arms. Rust side unchanged. Percolator's `AccountIdx = Fin[MAX_ACCOUNTS]` alias arrives correctly typed; promotion is a no-op there. |
| **D** — cover witness `1` for `Pubkey` field | `WitnessState::resolve_value`'s fallback returned `"1"` for unresolved values, poisoning Pubkey-typed fields in cover-theorem struct literals (`failed to synthesize OfNat Pubkey 1`). | Same `is_account_binding_pubkey_ref` skip in `WitnessState::apply` keeps the field at its `pk` default. Same root cause as B; one helper fixes both. |

### Lakefile / Proofs roots

`init.rs:356` was emitting `lean_lib <Name>Spec where roots := #[\`Spec]`
on every scaffolded project. Result: every user's hand-written
`Proofs.lean` was silently excluded from `lake build`. Anyone could
replace their `Proofs.lean` with `theorem nonsense : 0 = 1 := sorry`
and `lake build` would still exit 0. Verification-soundness gap.

- **`init.rs`** now emits `roots := #[\`Spec, \`Proofs]`. New scaffolds
  type-check proofs alongside the spec.
- **Existing example lakefiles updated** for escrow, escrow-split,
  and multisig (lending stays at `\`Spec` only — older single-file
  layout, no `Proofs.lean`).
- **multisig was the silent victim.** Its `Proofs.lean` had 14
  commented-out theorem signatures, never touched since scaffold.
  v2.11.3 fills 12 of them with real proofs and narrows
  `votes_bounded`'s `preserved_by` from `all` to an explicit list
  excluding the two handlers (`approve` / `reject`) that need an
  auxiliary count-by-predicate invariant the DSL can't yet express.
  Spec comment flags this for revisit.

### Multisig lakefile slice swap

multisig's `Spec.lean` imports `Mathlib.Algebra.BigOperators.Fin` +
`QEDGenMathlib.IndexedState` (Map-backed forall predicates) but its
lakefile required the base `qedgenSupport` slice — `lake build`
failed at `import Mathlib...` before any typechecking. Swapped to
`qedgenSupportMathlib` (matches percolator's idiom).

### Drift hash alignment

`drift.rs::content_hash` used `to_token_stream().to_string()`;
`qedgen-macros::verified::content_hash` uses a hand-rolled
`canonical_token_string` walker. The strings diverge subtly
(rustc-vs-`from_str` per-`Punct` `Spacing` handling), so
`qedgen check --update-hashes` was writing hashes the proc-macro then
*immediately rejected* on the next build — a documented footgun
(memory `project_drift_hash_divergence`).

v2.11.3 delegates `drift::content_hash` to the shared
`spec_hash::body_hash_for_fn`, which is byte-equivalent to the macro
by construction. New regression test
(`content_hash_equals_spec_hash_body_hash`) locks the alignment.

### Pre-release gate: `scripts/check-lake-build.sh`

Iterates every `examples/*/formal_verification/` (rust + sBPF), runs
`lake build`, exits 1 on any failure. `--strict` also fails on
missing `.lake/`/manifests for cold-checkout / release-day runs;
`--only <pattern>` filters to a subset. Wired into CLAUDE.md item 6
of the pre-release checklist; replaces the prose "run lake build in
each directory" with the explicit script.

CI not yet wired — Lean toolchain provisioning + Mathlib caching in
GitHub Actions is a separate piece of work. Manual release-day step
until then.

### Aristotle docs

`references/cli.md` was missing flag tables for `aristotle status`,
`aristotle result`, and `aristotle list`, and collapsed `cancel` /
`list` into one section. Each subcommand now has its own flag table.
Also added the `[5, 3600]` clamp note on `--poll-interval`. Replaced
an opaque `aristotlelib` source-comment with an explicit list of
packaging skip rules.

### README cleanup

- **Pinocchio promises removed.** README, `references/cli.md`,
  `references/qedspec-dsl.md`, `references/qedspec-imports.md`,
  `CLAUDE.md`, and CLI help text no longer claim "Pinocchio lands in
  v2.11+" / "ships in v2.10+". The enum value stays (clap surface
  preserved), but errors as "not yet implemented" instead of
  promising a version.
- **API-key claim corrected.** Quick-start and Requirements sections
  no longer say API keys are "set up automatically." Lean and Kani
  toolchains auto-install; API keys must be obtained from the
  providers and exported by the user.
- **Release-gates section** now lists `scripts/check-lake-build.sh`
  alongside the existing version-consistency / README-drift /
  regen-drift gates.
- **Quasar greenfield** has its own section (`Greenfield — Anchor or
  Quasar`) showing the parallel `--target quasar` invocation. The
  brownfield section is retitled "Existing Anchor programs" to
  reflect that `qedgen adapt` is Anchor-only today.
- **Examples list** adds escrow-split and the
  `examples/quasar-readiness/` ratchet demo.

## Compatibility

- DSL surface, parser, and `.qedspec` file format are unchanged.
- All Lean codegen behavior changes are additive — examples that
  were previously green stay green (they pass through the new code
  paths without producing different output, verified by
  `qedgen check --regen-drift` clean across all 5 driven examples).
- `Target::Pinocchio` enum variant retained; selecting it still
  errors (now with timeless wording).
- `qedgen check --update-hashes` now produces hashes the proc-macro
  accepts. Anyone who hit the divergence pre-v2.11.3 should re-run
  the command and commit the corrected values.
- multisig's `votes_bounded preserved_by` narrowed — anyone who had
  somehow been generating preservation harnesses for `approve` /
  `reject` (stuck on the "spec language can't express the auxiliary
  invariant" wall) will get a smaller harness set, not a different
  one.

## Pre-release gates (all clean)

- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 457 / 457 pass (added 5 over v2.11.2: 3 for codegen
  A+C, 2 for codegen B+D + drift hash alignment)
- `bash scripts/check-version-consistency.sh` — `2.11.3` consistent
- `bash scripts/check-readme-drift.sh` — clean
- `bash scripts/check-lake-build.sh` — **10 / 10 examples build clean**
- `qedgen check --regen-drift` — 5 / 5 examples in sync
- `cargo check` per bundled example — clean

## Carried forward (unchanged from v2.11.2)

- Lean coverage status flowing into `UnifiedReport::issue_count`
  causes `qedgen check --code` to exit 1 even on a clean spec when
  the bundled `Spec.lean` has stub `:= trivial` theorems with names
  that don't match `generate_properties` output. v3.0 cleanup item.
- `Target::Pinocchio` codegen still not implemented; reserved CLI
  surface only.
