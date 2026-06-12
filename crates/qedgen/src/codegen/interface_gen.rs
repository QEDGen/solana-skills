//! Tier-0 interface generator — Anchor IDL → `.qedspec` interface block.
//!
//! Emits a shape-only `interface Name { ... }` block: program ID, per-handler
//! discriminators, account roles (signer/writable), and argument types. No
//! `requires`/`ensures`/`effect` — those require semantic understanding that
//! an IDL alone cannot give. The upstream block is left as a TODO for humans
//! to fill in after they've verified the deployed program.
//!
//! See docs/design/spec-composition.md §2 "Tier 0 — shape from IDL."

use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::Path;

use crate::idl::{self, Idl, IdlAccount, IdlInstruction};

/// Generate an interface `.qedspec` from an Anchor IDL. Returns the rendered
/// source; the caller writes it to disk.
pub fn generate(idl_path: &Path) -> Result<String> {
    let (idl, _analyses) = idl::parse_idl(idl_path)?;
    Ok(render(&idl))
}

/// Convenience: generate + write in one step.
pub fn generate_to_file(idl_path: &Path, out_path: &Path) -> Result<()> {
    let src = generate(idl_path)?;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    std::fs::write(out_path, &src).with_context(|| format!("writing {}", out_path.display()))?;
    Ok(())
}

fn render(idl: &Idl) -> String {
    let program_name = &idl.metadata.name;
    let interface_name = snake_to_pascal(program_name);
    let spec_name = format!("{}Interface", interface_name);

    let mut out = String::new();
    writeln!(out, "// Tier-0 interface generated from Anchor IDL.").unwrap();
    writeln!(
        out,
        "// Shape only — no requires/ensures. Upgrade to Tier 1 by declaring"
    )
    .unwrap();
    writeln!(
        out,
        "// what each handler does to caller-observable state. See"
    )
    .unwrap();
    writeln!(
        out,
        "// docs/design/spec-composition.md §2 for the tier model."
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "spec {}", spec_name).unwrap();
    writeln!(out).unwrap();
    writeln!(out, "interface {} {{", interface_name).unwrap();

    if let Some(addr) = idl.address.as_deref() {
        writeln!(out, "  program_id \"{}\"", addr).unwrap();
        writeln!(out).unwrap();
    } else {
        writeln!(
            out,
            "  // TODO program_id — not present in IDL. Fill in the deployed address."
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    writeln!(out, "  upstream {{").unwrap();
    writeln!(
        out,
        "    // TODO fill in after you've run QEDGen harnesses against the"
    )
    .unwrap();
    writeln!(
        out,
        "    // deployed program. `binary_hash` is authoritative — it pins"
    )
    .unwrap();
    writeln!(
        out,
        "    // callers to the exact bytes on chain. `verified_with` lists"
    )
    .unwrap();
    writeln!(
        out,
        "    // only backends that were actually run; omit \"lean\" unless you"
    )
    .unwrap();
    writeln!(out, "    // have a real proof (not just axiomatization).").unwrap();
    writeln!(out, "    //").unwrap();
    writeln!(out, "    // package      \"{}\"", program_name).unwrap();
    writeln!(out, "    // version      \"TODO\"").unwrap();
    writeln!(out, "    // binary_hash  \"sha256:TODO\"").unwrap();
    writeln!(out, "    // verified_with [\"proptest\"]").unwrap();
    writeln!(out, "    // verified_at  \"TODO\"").unwrap();
    writeln!(out, "  }}").unwrap();

    for ix in &idl.instructions {
        writeln!(out).unwrap();
        render_handler(&mut out, ix);
    }

    writeln!(out, "}}").unwrap();
    out
}

fn render_handler(out: &mut String, ix: &IdlInstruction) {
    let name = &ix.name;

    // Doc comment — each line becomes `/// ...`.
    for line in &ix.docs {
        writeln!(out, "  /// {}", line.trim()).unwrap();
    }

    // handler signature: `handler name (p : T) (q : U)` — params, no transition.
    write!(out, "  handler {}", name).unwrap();
    for arg in &ix.args {
        let ty = idl::type_label(&arg.ty);
        write!(out, " ({} : {})", arg.name, ty).unwrap();
    }
    writeln!(out, " {{").unwrap();

    // discriminant: 8-byte Anchor discriminators render as a hex literal
    // (0x + 16 hex chars) so they match the format used in hand-authored
    // interfaces.
    if !ix.discriminator.is_empty() {
        let mut hex = String::from("0x");
        for b in &ix.discriminator {
            write!(hex, "{:02X}", b).unwrap();
        }
        writeln!(out, "    discriminant \"{}\"", hex).unwrap();
    } else {
        writeln!(
            out,
            "    // TODO discriminant — IDL did not carry one (pre-Anchor 0.30?)."
        )
        .unwrap();
    }

    // accounts { ... }
    if !ix.accounts.is_empty() {
        writeln!(out, "    accounts {{").unwrap();
        for acc in &ix.accounts {
            render_account(out, acc);
        }
        writeln!(out, "    }}").unwrap();
    }

    writeln!(out, "  }}").unwrap();
}

fn render_account(out: &mut String, acc: &IdlAccount) {
    let mut attrs: Vec<&str> = Vec::new();
    if acc.signer {
        attrs.push("signer");
    }
    if acc.writable {
        attrs.push("writable");
    } else {
        attrs.push("readonly");
    }
    // PDA seeds aren't rendered declaratively yet — keep the shape honest
    // and emit a comment pointing at them so humans can lift them if they
    // want the caller to check derivation.
    write!(out, "      {:<24} : {}", acc.name, attrs.join(", ")).unwrap();
    writeln!(out).unwrap();
    if acc.pda.is_some() {
        writeln!(
            out,
            "      // ^ IDL declares PDA seeds for {}; add `pda [...]` if callers should verify.",
            acc.name
        )
        .unwrap();
    }
}

fn snake_to_pascal(s: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for ch in s.chars() {
        if ch == '_' || ch == '-' {
            upper_next = true;
        } else if upper_next {
            out.push(ch.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_interface_parses_back() {
        // Use the escrow IDL shipped under examples/ as the fixture. If the
        // generator ever emits invalid .qedspec, this round-trip fails loud.
        let idl_path = Path::new("../../examples/rust/escrow/target/idl/escrow.json");
        if !idl_path.exists() {
            // Escrow IDL is a build artifact; skip when it hasn't been built.
            eprintln!("skipping: {} not present", idl_path.display());
            return;
        }
        let rendered = generate(idl_path).expect("IDL generator produced output");

        // Round-trip through the parser — any grammar slip shows up here.
        let parsed = crate::chumsky_adapter::parse_str(&rendered)
            .expect("generated interface re-parses cleanly");
        assert_eq!(
            parsed.interfaces.len(),
            1,
            "expected exactly one interface block"
        );
        let iface = &parsed.interfaces[0];
        assert_eq!(iface.name, "Escrow");
        assert_eq!(
            iface.program_id.as_deref(),
            Some("FyeRokiKoSz9VxRdgDEuKVKwWuGsZLEbMkywgJQDXeFK")
        );
        // Tier 0: all handlers have empty requires/ensures.
        for h in &iface.handlers {
            assert!(h.requires.is_empty(), "{} leaked a requires", h.name);
            assert!(h.ensures.is_empty(), "{} leaked an ensures", h.name);
            assert!(h.discriminant.is_some(), "{} missing discriminant", h.name);
        }
    }

    #[test]
    fn snake_to_pascal_basic() {
        assert_eq!(snake_to_pascal("spl_token"), "SplToken");
        assert_eq!(snake_to_pascal("escrow"), "Escrow");
        assert_eq!(snake_to_pascal("my_program"), "MyProgram");
    }
}
