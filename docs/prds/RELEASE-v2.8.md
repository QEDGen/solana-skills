# QEDGen v2.8 — release notes draft

**Tag:** `v2.8.0` (pending)
**Branch:** `v2.8-cpi-import` (10 commits ahead of `main`)
**Theme:** spec dependencies, end-to-end.

This is the spec-composition release. The CPI gap that's been the
"yeah, but…" of every brownfield eval since v2.5 is closed. Specs that
call into SPL Token (or another team's qedspec) declare deps in
`qed.toml`, import them by name, and get real Anchor CPI codegen + Lean
ensures-as-axiom theorems + supply-chain pinning end-to-end.

## What's in

### G1 — `import` + `qed.toml` manifest + resolver

- New top-level statement: `import Token from "spl_token"`. Bound name
  must match an `interface Token { ... }` block in the imported source.
- New `qed.toml` manifest at the spec root, cargo-style:
  ```toml
  [dependencies]
  spl_token = { github = "QEDGen/solana-skills", path = "interfaces/spl_token", tag = "v2.8.0" }
  my_amm    = { path = "../my_amm" }
  ```
- GitHub source: shells out to `git clone --depth=1 --branch <ref>`
  (or full clone + checkout for commit refs). Cached at
  `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/`.
- Path source: relative to manifest dir, with `.qedspec`
  auto-extension fallback.
- Pipeline integration: `parse_spec_file` resolves imports after parse
  and merges matching interfaces into `parsed.interfaces`.

### G2 — `qed.lock` + `--frozen`

- Auto-written snapshot of resolved imports. Pins both `ref` and
  `resolved_commit` so force-pushed tags get caught.
- `qedgen check --frozen` errors with a per-dep diff if the on-disk
  lock differs from the freshly computed one. CI flag.
- `examples/rust/escrow-split/qed.lock` committed as the canonical
  regression baseline.

### G3 — Lean ensures-as-axiom CPI theorems

- `render_cpi_theorems` rewritten. For each `call Interface.handler(...)`
  site, look up the callee, substitute call-site args into each
  `ensures` clause, and emit:
  ```lean
  theorem <op>_<iface>_<handler>_call_<idx>_post_<i> (s : State) <handler_params>
    : <substituted_ensures> := by sorry
  ```
- Stance-1 axiomatization. The `sorry` is the contract boundary —
  v3.0 stance 2 will close it via imported callee proofs.
- Bound-identifier handling: state-field references prefixed with
  `s.`; handler params declared explicitly in the theorem signature.
- The pre-2.8 `: True := trivial` rubber stamp is gone.

### G4 — Anchor CPI codegen for SPL Token transfer

- `call Token.transfer(...)` against the canonical SPL Token program
  ID (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`) emits a real
  `anchor_spl::token::transfer(CpiContext::new(...), <amount>)?` body.
- State-field amount references resolve to `self.<state_acct>.<field>`;
  handler params bind via the fn signature.
- escrow-split's exchange handler ships with no `todo!()` for either
  Token.transfer call site.
- Other SPL Token handlers (`mint_to`, `burn`, `initialize_account`,
  `close_account`) and non-SPL interfaces remain on the comment +
  `todo!()` path — v2.9 scope.

### G5 — `qedgen verify --check-upstream`

- New flag: diffs every imported library's pinned
  `upstream_binary_hash` against the on-chain `.so`.
- Implementation shells out to `solana program dump --url <rpc> <id>
  <tmpfile>` per `feedback_dispatch_over_reimplement.md`. Zero new
  client crates added; `--rpc-url` passes through, otherwise inherits
  from `~/.config/solana/cli/config.yml`.
- Per-dep outcomes: Match / Mismatch / Skipped / Error. Non-zero exit
  on any failure.

### G6 + G7 — docs

- New `references/qedspec-imports.md` covering the full
  `import` + `qed.toml` + `qed.lock` + `--frozen` + `--check-upstream`
  surface.
- `docs/design/spec-composition.md` migrated to ` ```fsharp ` fences
  for syntax highlighting. F# is the closest existing highlighter to
  qedspec syntax (`//` comments, `type T = | A | B of …` variants).
- `SKILL.md` and `CLAUDE.md` updated for the new ensures-as-axiom
  story and the `--frozen` pre-release gate.

## What's not in

Honest about scope. v2.9 candidates:

- **Transitive resolution.** Imported specs that themselves use
  `import` aren't walked. Each consumer declares its own direct deps.
- **Multi-file imported deps.** Resolver supports directory mode but
  the merge into the consumer requires single-file.
- **Generic Anchor CPI codegen.** Only SPL Token transfer is
  mechanized. Other handlers and non-SPL interfaces stay on
  comment + `todo!()`.
- **`program_id` in `qed.lock`.** `--check-upstream` skips deps when
  the program_id can't be reached from the lock alone.
- **Framework detection (`Anchor.toml` walk).** Codegen always emits
  Anchor today — explicit detection is a v2.9 polish.
- **Stance 2 (proof composition).** `sorry` in ensures-as-axiom
  theorems stays — v3.0 with the Anchor adapter.

## Numbers

- 10 commits on `v2.8-cpi-import` (G1: 4, G2: 2, G3: 1, G4: 1, G5: 1,
  rebased-in const fix #10).
- +65 tests (319/319 pass; baseline 254).
- Zero new dependency surface modulo `toml`, `tempfile`. No
  `solana-client`, no `git2`.
- `cargo fmt --check` clean. `cargo clippy -- -D warnings` clean.

## End-to-end demo

```bash
$ qedgen check --spec examples/rust/escrow-split/
# parses, resolves Token via qed.toml, writes qed.lock if changed
$ qedgen check --spec examples/rust/escrow-split/ --frozen
# CI mode: errors if lock stale
$ qedgen verify --spec examples/rust/escrow-split/escrow.qedspec --check-upstream
  · spl_token: skipped — no upstream_binary_hash pinned
$ qedgen codegen --spec examples/rust/escrow-split/
# produces real anchor_spl::token::transfer CPI bodies in handler scaffolds
# produces real Lean ensures-as-axiom theorems in formal_verification/Spec.lean
```

## Pre-tag gates

- [x] `cargo fmt --check`
- [x] `cargo clippy -- -D warnings`
- [x] `cargo test` — 319/319
- [ ] `bash scripts/check-readme-drift.sh`
- [ ] `lake build` in every `examples/*/formal_verification/`
- [ ] `qedgen check --frozen --spec examples/rust/escrow-split/`
- [ ] Bump `crates/qedgen/Cargo.toml` to `2.8.0`
- [ ] Tag + `gh release create v2.8.0`
