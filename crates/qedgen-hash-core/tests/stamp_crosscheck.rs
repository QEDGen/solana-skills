//! T3 "hashes didn't move" proof (finding F1).
//!
//! Re-derives checked-in `#[qed(verified, hash = …, spec_hash = …)]`
//! stamps from `examples/` through the shared hash-core implementation
//! and hard-asserts the exact hex values. These stamps were computed by
//! the pre-extraction mirror copies (macro side at compile time, CLI
//! side via `--update-hashes` / `adapt`), so equality here proves the
//! extraction is byte-identical on real inputs — the free-fn Anchor
//! shape, the impl-method Quasar shape, and the ADT-state example.
//!
//! If this test fails after a change to `qedgen-hash-core`, the change
//! altered hash behavior: every stamped program in the wild would fire
//! spurious `compile_error!` drift. Don't update the constants — revert
//! the behavior.

use std::path::{Path, PathBuf};

use quote::ToTokens;

use qedgen_hash_core::{spec_hash_for_handler, token_stream_hash};

struct Stamp {
    /// Rust file (repo-relative) carrying the `#[qed(verified, …)]` stamp.
    rust_file: &'static str,
    /// Expected `hash = "…"` (body hash) — the value checked into the stamp.
    body_hash: &'static str,
    /// Spec leg: `(spec file repo-relative, handler name, expected spec_hash)`.
    /// `None` for body-only stamps (legacy `#[qed(verified, hash = …)]`).
    spec: Option<(&'static str, &'static str, &'static str)>,
}

/// Stamps across four example programs plus the compile-gated drift
/// fixture, covering: free fns (fixture `deposit`/`withdraw`), impl
/// methods (fixture `process`; escrow exchange, multisig, lending),
/// and the sole ADT-state example (cross-program-vault).
///
/// Deliberately NOT asserted (pre-existing staleness on main, verified
/// against the v2.40.0 pre-extraction binary — NOT hash-core divergence;
/// these examples are workspace-`exclude`d so the macro never gates them):
///   - the legacy body-only stamps in
///     `examples/rust/escrow/programs/escrow/src/lib.rs` (stamped at the
///     macro crate's introduction, pre-v2.15 canonicalization; `check
///     --drift` on main reports all three DRIFTED),
///   - the `spec_hash` leg of escrow `programs/src` (`reconcile` on
///     main reports it SPEC HASH DRIFT; its body leg verifies clean
///     and IS asserted below).
const STAMPS: &[Stamp] = &[
    Stamp {
        // Free fn; compile-gated on every `cargo build` (workspace member).
        rust_file: "crates/qed-drift-fixture/src/lib.rs",
        body_hash: "cd876f6bf941e7f0",
        spec: Some((
            "crates/qed-drift-fixture/example.qedspec",
            "deposit",
            "557f689570be9221",
        )),
    },
    Stamp {
        // Impl method (Marinade/Squads-shape handler); also compile-gated.
        rust_file: "crates/qed-drift-fixture/src/lib.rs",
        body_hash: "480a7764187e2bc6",
        spec: Some((
            "crates/qed-drift-fixture/example.qedspec",
            "process",
            "f244fe26fd21aac4",
        )),
    },
    Stamp {
        rust_file: "examples/rust/escrow/programs/src/instructions/exchange.rs",
        body_hash: "7b560f1c9c9b8b97",
        spec: None, // spec_hash leg stale on main — see module doc
    },
    Stamp {
        rust_file: "examples/rust/multisig/programs/src/instructions/approve.rs",
        body_hash: "659801016cb87703",
        spec: Some((
            "examples/rust/multisig/multisig.qedspec",
            "approve",
            "96727ecc91c0452e",
        )),
    },
    Stamp {
        rust_file: "examples/rust/lending/programs/src/instructions/repay.rs",
        body_hash: "6c160536ed8b56b1",
        spec: Some((
            "examples/rust/lending/lending.qedspec",
            "repay",
            "9b5d2eb1d0b2b787",
        )),
    },
    Stamp {
        rust_file: "examples/rust/cross-program-vault/programs/src/instructions/deposit.rs",
        body_hash: "ba56825b97e00c86",
        spec: Some((
            "examples/rust/cross-program-vault/vault.qedspec",
            "deposit",
            // Re-pinned when `deposit` gained the explicit `permissionless`
            // marker (post-v2.44.0 lint cleanup); CLI `--update-hashes` and
            // hash-core agreed on this value at the re-stamp.
            "1fa96b38d214b3c3",
        )),
    },
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// True when one of `attrs` is a `#[qed(…)]` whose tokens mention `stamp`.
/// The stamped hex is how we locate the exact fn the stamp seals.
fn has_stamp(attrs: &[syn::Attribute], stamp: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("qed") && a.to_token_stream().to_string().contains(stamp))
}

/// Recompute the body hash of the fn carrying `stamp`, exactly as both
/// the proc-macro (`FnLike::content_hash`) and the CLI
/// (`spec_hash::body_hash_for_*`) do: strip all outer attributes, hash
/// the canonical token stream. Walks free fns, impl methods, and inline
/// mods.
fn body_hash_of_stamped_fn(items: &[syn::Item], stamp: &str) -> Option<String> {
    for item in items {
        match item {
            syn::Item::Fn(f) if has_stamp(&f.attrs, stamp) => {
                let mut stripped = f.clone();
                stripped.attrs.clear();
                return Some(token_stream_hash(&stripped.to_token_stream()));
            }
            syn::Item::Impl(imp) => {
                for ii in &imp.items {
                    if let syn::ImplItem::Fn(m) = ii {
                        if has_stamp(&m.attrs, stamp) {
                            let mut stripped = m.clone();
                            stripped.attrs.clear();
                            return Some(token_stream_hash(&stripped.to_token_stream()));
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, sub)) = &m.content {
                    if let Some(h) = body_hash_of_stamped_fn(sub, stamp) {
                        return Some(h);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn shared_impl_reproduces_checked_in_stamps() {
    let root = repo_root();
    for s in STAMPS {
        // Body-hash leg.
        let rust_path = root.join(s.rust_file);
        let rust_src = std::fs::read_to_string(&rust_path)
            .unwrap_or_else(|e| panic!("read {}: {}", rust_path.display(), e));
        let file: syn::File = syn::parse_file(&rust_src)
            .unwrap_or_else(|e| panic!("parse {}: {}", rust_path.display(), e));
        let actual_body = body_hash_of_stamped_fn(&file.items, s.body_hash).unwrap_or_else(|| {
            panic!(
                "no fn stamped with hash {} found in {}",
                s.body_hash, s.rust_file
            )
        });
        assert_eq!(
            actual_body, s.body_hash,
            "body hash moved for {} — hash-core diverged from the \
             pre-extraction implementation",
            s.rust_file
        );

        // Spec-hash leg.
        if let Some((spec_file, handler, expected_spec)) = s.spec {
            let spec_path = root.join(spec_file);
            let spec_src = std::fs::read_to_string(&spec_path)
                .unwrap_or_else(|e| panic!("read {}: {}", spec_path.display(), e));
            let actual_spec = spec_hash_for_handler(&spec_src, handler)
                .unwrap_or_else(|| panic!("handler {} not found in {}", handler, spec_file));
            assert_eq!(
                actual_spec, expected_spec,
                "spec hash moved for handler {} in {} — hash-core diverged \
                 from the pre-extraction implementation",
                handler, spec_file
            );
        }
    }
}

/// Accounts-struct leg: the drift fixture pins
/// `accounts_hash = "46abbeeb1ecb80d9"` for `struct Vault` — recompute it
/// the way both `qedgen::spec_hash::accounts_struct_hash` and
/// `qedgen-macros::spec_bind::accounts_struct_hash_in` do (strip outer
/// attrs, hash the canonical token stream).
#[test]
fn shared_impl_reproduces_fixture_accounts_hash() {
    let path = repo_root().join("crates/qed-drift-fixture/src/lib.rs");
    let src = std::fs::read_to_string(&path).expect("read drift fixture");
    let file: syn::File = syn::parse_file(&src).expect("parse drift fixture");
    let strukt = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(s) if s.ident == "Vault" => Some(s.clone()),
            _ => None,
        })
        .expect("struct Vault in drift fixture");
    let mut stripped = strukt;
    stripped.attrs.clear();
    assert_eq!(
        token_stream_hash(&stripped.to_token_stream()),
        "46abbeeb1ecb80d9",
        "accounts-struct hash moved — hash-core diverged from the \
         pre-extraction implementation"
    );
}
