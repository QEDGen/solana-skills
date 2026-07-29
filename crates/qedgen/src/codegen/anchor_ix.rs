//! Anchor instruction encoding, shared by every emitter that has to build
//! an Anchor-shaped transaction by hand.
//!
//! Anchor's wire format is fully determined by the spec, so this is
//! mechanical translation, not business logic:
//!
//! - instruction data: `sha256("global:<handler>")[..8]`, then the
//!   arguments Borsh-encoded in declaration order;
//! - account data: `sha256("account:<Struct>")[..8]`, then the fields;
//! - account metas: the handler's `accounts` block in order, carrying its
//!   signer / writable flags.
//!
//! Two lanes need this: the Parallax reproducer (`parallax_repro`) and the
//! Parallax integration scaffold (`integration_test`, #366). They had one
//! copy between them, which is one more than the discriminator rule should
//! ever have.
//!
//! ## Borsh and the Pod layout agree here
//!
//! Argument and field bytes come from
//! [`crate::codegen_shared::emit_pod_bytes_append`], which encodes Quasar's
//! zero-copy Pod layout. For the fixed types either lane can carry —
//! integers, `Bool`, addresses, `Bytes32`/`Bytes64`, and arrays of those —
//! Borsh and Pod are the same bytes: little-endian, one byte for a bool,
//! 32 for an address, elements back to back with no padding and no length
//! prefix. They diverge only on `Vec`, `Option`, and `String`, which that
//! function refuses for both.
//!
//! So there is one encoder, not two, and the place it would stop being
//! true is the place it already errors.

/// Render `sha256(<preimage>)[..8]` as a Rust byte-array literal.
///
/// Computed here and emitted as literal bytes rather than hashed at
/// runtime: the value can never change, and a generated repro crate is
/// standalone, so every dependency it names has to be declared in a
/// manifest the generator also writes.
pub(crate) fn discriminator_literal(preimage: &str) -> String {
    let hex = qedgen_hash_core::sha256_hex16(preimage);
    let bytes: Vec<String> = (0..8)
        .map(|index| format!("0x{}", &hex[index * 2..index * 2 + 2]))
        .collect();
    format!("[{}]", bytes.join(", "))
}

/// The instruction discriminator preimage for a handler.
pub(crate) fn instruction_preimage(handler: &str) -> String {
    format!("global:{handler}")
}

/// The account discriminator preimage for a generated state struct.
pub(crate) fn account_preimage(struct_name: &str) -> String {
    format!("account:{struct_name}")
}

/// One `AccountMeta` for the instruction's account list.
///
/// `is_signer` is passed rather than read off the account because callers
/// vary it: the reproducer lane strips every signer flag to prove an
/// absent authority gate, while the integration scaffold uses the flags
/// the spec declares.
pub(crate) fn account_meta_expr(
    account: &crate::check::ParsedHandlerAccount,
    is_signer: bool,
) -> String {
    // Programs resolve to their runtime ids rather than a fixture.
    let address = if account.is_program {
        if account.name.contains("system") {
            "system_program::ID".to_string()
        } else if account.name.contains("token") {
            "SPL_TOKEN_PROGRAM_ID".to_string()
        } else {
            format!("{}, /* AGENT: program id */", account.name)
        }
    } else {
        account.name.clone()
    };

    if account.is_writable {
        format!("AccountMeta::new({address}, {is_signer})")
    } else {
        format!("AccountMeta::new_readonly({address}, {is_signer})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two preimages are the whole Anchor ABI contract here. Pinned as
    /// literals so a change to either is a deliberate edit with a failing
    /// test, not a silent re-derivation.
    #[test]
    fn preimages_follow_the_anchor_convention() {
        assert_eq!(instruction_preimage("add_member"), "global:add_member");
        assert_eq!(account_preimage("VaultAccount"), "account:VaultAccount");
    }

    /// Eight bytes, hex-escaped, in `sha256` order.
    #[test]
    fn discriminator_is_eight_hex_bytes() {
        let rendered = discriminator_literal("global:deposit");
        assert!(rendered.starts_with("[0x") && rendered.ends_with(']'));
        assert_eq!(rendered.matches("0x").count(), 8);
    }

    #[test]
    fn account_metas_follow_writability_and_the_passed_signer_flag() {
        let account =
            |name: &str, signer: bool, writable: bool| crate::check::ParsedHandlerAccount {
                name: name.to_string(),
                is_signer: signer,
                is_writable: writable,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            };

        assert_eq!(
            account_meta_expr(&account("owner", true, true), true),
            "AccountMeta::new(owner, true)"
        );
        assert_eq!(
            account_meta_expr(&account("vault", false, false), false),
            "AccountMeta::new_readonly(vault, false)"
        );
        // The caller's flag wins over the declared one — that is how the
        // reproducer lane forges an unsigned invocation.
        assert_eq!(
            account_meta_expr(&account("owner", true, true), false),
            "AccountMeta::new(owner, false)"
        );
    }
}
