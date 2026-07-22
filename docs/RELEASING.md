# Releasing QEDGen

Pre-release checklist. Run before cutting a new release or tag. (Moved out of `CLAUDE.md` so it isn't loaded into every session — it only matters at release time.)

1. **Bump version** in `crates/qedgen/Cargo.toml`, `package.json`, AND `skills/qedgen-auditor/VERSION` — `install.sh` derives its version from Cargo.toml; the `check-version-consistency.sh` CI gate fails the build if the first two drift (v2.28.0 shipped with this exact mismatch; v2.28.1 hotfixed it), and `check-auditor-skill.sh` fails it if the skill VERSION drifts from package.json. After bumping the skill VERSION, sync the installed copy — `bash scripts/sync-auditor-skill.sh .claude/skills/qedgen-auditor` — or the auditor-skill gate fails on the installed-copy diff (#261: every release hit one avoidable failed gate run here). Then run `bash scripts/check-version-consistency.sh && bash scripts/check-auditor-skill.sh` to confirm.

1a. **Re-stamp the version-pinned generated artifacts** — codegen stamps `qedgen-macros = { …, tag = "v<version>" }` into every generated `Cargo.toml`, so a version bump drifts BOTH the codegen snapshots AND the committed bundled examples. After bumping, run (rebuild `bin/qedgen` first): `UPDATE_SNAPSHOTS=1 cargo test --test codegen_snapshot` (refresh the 6 codegen fixtures) AND `qedgen check --regen-drift --write` (re-stamp the 8 `examples/rust/*/**/Cargo.toml` pins). Skipping this fails the `Run tests` (codegen_snapshot) + `Check example codegen drift` CI steps — v2.31 hit both in sequence. Verify each diff is *only* the tag line, then `cargo test` / `qedgen check --regen-drift` should be clean.

2. **`cargo fmt --check`** — matches the CI gate; `cargo test` does NOT run fmt, so this is an easy miss if skipped

3. **`cargo clippy -- -D warnings`** — matches the CI gate (plain `cargo clippy` is too lenient)

4. **`cargo test`** — all tests must pass

5. **`bash scripts/check-readme-drift.sh`** — CI runs this; catches undocumented CLI commands

6. **`bash scripts/check-lake-build.sh --strict`** — manual release gate (automatic Lean workflows are temporarily disabled). Runs `lake build` in every `examples/*/formal_verification/` (rust + sBPF) and exits 1 on any failure. `--strict` also fails on missing `.lake/`/manifests (cold checkout); drop `--strict` for a non-release sanity check. Run the equivalent `Lake build (Lean side)` workflow manually when local caches/toolchains are unavailable. v2.11.2 shipped two examples with broken `Spec.lean` because this gate didn't exist — `qedgen check --regen-drift` and `cargo check` only verify the Rust scaffold, not Lean.

7. **`bash scripts/release-gate.sh`** (#271) — the mechanical §7+§8 gate; CI also runs it on every PR, so at release time it should already be green. It asserts, by exit code and summary line (never by eyeball):
   - **Zero `sorry`** over example Lean proof artifacts (vendored `lean_solana/` support-package copies excluded). Only Tier-0 CPI theorems (callee declared no `ensures`) may carry `sorry`; those files match the `ensures @ \`` marker. Tier-1/2 CPI theorems apply `<Iface>.<handler>.ensures_axiom_<idx>` since v2.26 (Slice 4a); the P1 lint `cpi_no_callee_ensures` surfaces Tier-0 at check time.
   - **`qedgen check --frozen` baseline** for every `examples/rust/*/` with a `qed.toml`: error- and warning-clean (exit 0, `0 error(s), 0 warning(s)`) **except `multisig`**, which intentionally ships one P2 (`excluded_op_modifies_property` on `approve`/`reject` × `votes_bounded` — the preservation proof needs a count-by-predicate auxiliary invariant the DSL can't express yet; see the comment above the property in `multisig.qedspec`). Stale `qed.lock`s fail the frozen check too.

   Changing the baseline (new example, intentional new warning) means editing the expectation table in `scripts/release-gate.sh` in the same PR — the script, not this document, is the source of truth for expected outcomes.

8. *(folded into step 7 — kept numbered so 8a/8b references below stay stable)*

8a. **`old(...)` preservation harnesses (v2.23+)** — for every bundled spec whose `property` body contains `old(...)` (`grep -rl '\bold(' examples crates/qedgen/tests/fixtures --include='*.qedspec'`), regen and confirm `tests/proptest.rs` emits the binary signature (`fn <prop>(pre: &State, post: &State) -> bool`) and the per-handler harness captures `let pre = s.clone(); let mut post = s;` before the handler call. Pre-v2.23 this lowered to a structural tautology silently. Bundled coverage today: `crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec` is the canonical pre/post test corpus. Run the spot-check in a disposable git repo; harness-only codegen deliberately needs no `.qed/` initialization:

   ```bash
   spotcheck_dir="$(mktemp -d)"
   cp crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec "$spotcheck_dir/"
   git -C "$spotcheck_dir" init --quiet
   qedgen codegen --proptest --spec "$spotcheck_dir/pool.qedspec"
   grep -nE 'fn .*\(pre: &State, post: &State\)|let pre = s\.clone\(\);|let mut post = s;' "$spotcheck_dir/programs/tests/proptest.rs"
   ```

8b. **Supply-chain gate** — run the exact CI command below, then `cargo deny check`; both must exit 0. Install once with `cargo install --locked cargo-audit cargo-deny`. New RustSec advisories on transitive deps are the actionable signal; the ignored IDs are documented in `deny.toml`'s `[advisories].ignore` array — keep this command, CI, README, and `deny.toml` in sync.

   ```bash
   cargo audit --deny warnings \
     --ignore RUSTSEC-2024-0436 \
     --ignore RUSTSEC-2024-0388 \
     --ignore RUSTSEC-2025-0141 \
     --ignore RUSTSEC-2025-0161 \
     --ignore RUSTSEC-2026-0097
   cargo deny check
   ```

   The accepted advisories are `paste` and `derivative` (unmaintained Arkworks transitive dependencies), `bincode` and `libsecp256k1` (unmaintained Anza/Solana transitive dependencies), and `rand`'s custom-logger unsoundness (the triggering logger configuration is not used here). License allowlist + registry / git-source policy live in `deny.toml`.

9. **Doc/code drift sweep** — README, SKILL.md, CLAUDE.md, `references/`, `docs/design/`, this file, `docs/prds/RELEASE-v<version>.md`, and module `//!` docstrings all have to match shipped reality. The `check-readme-drift.sh` script only covers top-level command coverage in README; everything else needs an explicit pass. Concretely:
   - Every `Subcommand` arm in `crates/qedgen/src/main.rs` has a section in `references/cli.md`, with every flag in its `#[arg]` set documented.
   - No `references/`, README, SKILL.md, `.claude/rules/`, or `docs/prds/RELEASE-v<version>.md` page references symbols / files / flags that no longer exist (`grep` for the names of just-removed modules, types, fns, CLI flags).
   - No mention in user-facing docs of features the release doesn't ship (the RELEASE notes are the worst offender — bring the "What's in" list in line with the actual shipped commits).
   - `feedback_no_anchor_v2_mentions.md` policy: don't name external codebases as the **source of audit findings** (anchor-v2, named protocols like Marinade/Squads/Drift/Raydium/Jito) in SKILL.md, references/, RELEASE-v<version>.md, or `clap` help text — present findings as qedgen's own taxonomy. This does NOT cover frameworks we **actively integrate** as codegen / audit targets: Anchor, Quasar, and Pinocchio are first-class `--target` / `--runtime` values, so naming them (incl. `quasar_lang` / "Blueshift Quasar" in target help text) is correct and necessary. Internal-only (test fixtures, private comments) is fine.
   - `CLAUDE.md` stays slim (deep content lives in `references/` and `docs/design/`, not in CLAUDE.md). Tracked as uppercase `CLAUDE.md` since v2.35.0 — the lowercase "mirror" was a macOS case-insensitivity illusion (only one file was ever in git).
   - Module-level `//!` docstrings on files you touched in the release reflect current behavior — not the behavior pre-fix.

10. **Tag AND publish the GitHub release** — a pushed tag alone builds **no** binaries: `.github/workflows/release.yml` triggers on the **release-created event** (`on: release: types: [created]`), not on tag push (#261). So:

    ```bash
    git tag v<version> && git push origin v<version>
    gh release create v<version> --title "v<version> — <one-line theme>" --notes-file docs/prds/RELEASE-v<version>.md
    ```

    `gh release create` fires `release.yml`, which cross-compiles the `qedgen-<arch>-<os>` assets + `.sha256` checksums that `install.sh` downloads (pinned to this exact tag). Verify the assets are attached before announcing — an assetless release makes every fresh `install.sh` fall back to source builds (and the weekly `cold-start.yml` run will fail on the download path).
