//! Inference tests: embedded Pinocchio source/ABI fixtures fed through the
//! profile inferer, asserting recovered accounts, params, tags, and PDAs.

use super::*;

#[test]
fn infers_account_order_params_and_dispatch_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("instructions")).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        r#"
pub fn process_instruction(
_program_id: &pinocchio::pubkey::Pubkey,
accounts: &[AccountInfo],
instruction_data: &[u8],
) -> ProgramResult {
let (discriminant, data) = instruction_data.split_first().unwrap();
match *discriminant {
    7 => instructions::transfer::process_transfer(accounts, data),
    _ => Err(ProgramError::InvalidInstructionData),
}
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("instructions/transfer.rs"),
        r#"
pub fn process_transfer(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
let [source, destination, authority, mint, token_program, ..] = accounts else {
    return Err(ProgramError::NotEnoughAccountKeys);
};
if !authority.is_signer() {
    return Err(ProgramError::MissingRequiredSignature);
}
if !source.is_writable() {
    return Err(ProgramError::InvalidAccountData);
}
require_token_account(source, mint.key(), authority.key())?;
let decimals = read_mint_decimals(mint)?;
require_key(token_program, &SPL_TOKEN_ID)?;
let amount = u64::from_le_bytes(
    instruction_data
        .get(0..8)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?,
);
let lane = u8::from_le_bytes(
    instruction_data
        .get(8..9)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?,
);
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");
    assert_eq!(transfer.instruction_tag, Some(7));
    assert_eq!(
        transfer.accounts,
        [
            "source",
            "destination",
            "authority",
            "mint",
            "token_program"
        ]
    );
    assert_eq!(
        transfer.params,
        [
            PinocchioParamField {
                name: "amount".to_string(),
                rust_type: "u64".to_string(),
                start: 0,
                end: 8
            },
            PinocchioParamField {
                name: "lane".to_string(),
                rust_type: "u8".to_string(),
                start: 8,
                end: 9
            }
        ]
    );
    assert_eq!(
        transfer.account_roles.get("authority"),
        Some(&PinocchioAccountRole {
            is_signer: Some(true),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.account_roles.get("source"),
        Some(&PinocchioAccountRole {
            is_writable: Some(true),
            account_type: Some("token".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.account_roles.get("mint"),
        Some(&PinocchioAccountRole {
            account_type: Some("mint".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.account_roles.get("token_program"),
        Some(&PinocchioAccountRole {
            is_program: Some(true),
            account_type: Some("token".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.token_account_bindings.get("source"),
        Some(&PinocchioTokenAccountBinding {
            mint_account: Some("mint".to_string()),
            owner_account: Some("authority".to_string()),
            owner_key_derivation: None,
        })
    );
    assert!(profile.pda_derivations.is_empty());
}

#[test]
fn unparseable_source_fails_loudly_instead_of_under_inferring() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Missing closing brace: `syn::parse_file` fails. The old regex fallback
    // would have silently under-inferred a partial profile from this file.
    std::fs::write(
        src.join("processor.rs"),
        r#"
pub fn process_transfer(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [source, destination, authority, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    Ok(())
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("ok.rs"),
        "pub fn process_ping(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult { Ok(()) }\n",
    )
    .unwrap();

    let err = infer_from_src_dir(&src).expect_err("unparseable Rust must be a hard error");
    let message = format!("{err:#}");
    assert!(
        message.contains("unparseable Rust"),
        "error should state that the source failed to parse: {message}"
    );
    assert!(
        message.contains("processor.rs") && message.contains(&src.display().to_string()),
        "error should name the offending file: {message}"
    );
    assert!(
        !message.contains("ok.rs"),
        "error should only name files that failed to parse: {message}"
    );
}

#[test]
fn skips_generated_kani_impl_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("kani_impl.rs"),
        "pub fn process_fake(accounts: &[AccountInfo], instruction_data: &[u8]) {}\n",
    )
    .unwrap();
    let profile = infer_from_src_dir(&src).expect("profile");
    assert!(profile.handlers.is_empty());
    assert!(profile.pda_derivations.is_empty());
}

#[test]
fn infers_multiline_token_bindings_through_key_aliases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("processor.rs"),
        r#"
fn process_rebalance(accounts: &[AccountInfo]) -> ProgramResult {
let account_info_iter = &mut accounts.iter();
let mint = next_account_info(account_info_iter)?;
let source_authority = next_account_info(account_info_iter)?;
let source_inventory = next_account_info(account_info_iter)?;
let destination_inventory = next_account_info(account_info_iter)?;
let source_authority_key = derive_authority(0).0;
let destination_authority_key = derive_authority(1).0;
require_key(source_authority, &source_authority_key)?;
require_token_account(source_inventory, mint.key(), &source_authority_key)?;
require_token_account(
    destination_inventory,
    mint.key(),
    &destination_authority_key,
)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let rebalance = profile.handler("rebalance").expect("rebalance profile");
    assert_eq!(
        rebalance.token_account_bindings.get("source_inventory"),
        Some(&PinocchioTokenAccountBinding {
            mint_account: Some("mint".to_string()),
            owner_account: Some("source_authority".to_string()),
            owner_key_derivation: Some(PinocchioLocalKeyDerivation {
                derivation: "authority".to_string(),
                args: vec!["0".to_string()],
            }),
        })
    );
    assert_eq!(
        rebalance
            .token_account_bindings
            .get("destination_inventory"),
        Some(&PinocchioTokenAccountBinding {
            mint_account: Some("mint".to_string()),
            owner_account: None,
            owner_key_derivation: Some(PinocchioLocalKeyDerivation {
                derivation: "authority".to_string(),
                args: vec!["1".to_string()],
            }),
        })
    );
}

#[test]
fn infers_account_order_from_next_account_info_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("processor.rs"),
        r#"
fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
let account_info_iter = &mut accounts.iter();
let config = next_account_info(account_info_iter)?;
let mint = next_account_info(account_info_iter)?;
let token_program = next_account_info(account_info_iter)?;
let decimals = read_mint_decimals(mint)?;
require_key(token_program, &SPL_TOKEN_ID)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let route = profile.handler("route").expect("route profile");
    assert_eq!(route.accounts, ["config", "mint", "token_program"]);
    assert_eq!(
        route.account_roles.get("mint"),
        Some(&PinocchioAccountRole {
            account_type: Some("mint".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        route.account_roles.get("token_program"),
        Some(&PinocchioAccountRole {
            is_program: Some(true),
            account_type: Some("token".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
}

#[test]
fn infers_pda_derivations_from_source_and_schema_seed_literals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(dir.path().join("schema")).unwrap();
    std::fs::write(
        src.join("state.rs"),
        r#"
pub fn derive_config(program_id: &Pubkey) -> (Pubkey, u8) {
pinocchio::pubkey::find_program_address(&[CONFIG_SEED], program_id)
}

pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
pinocchio::pubkey::try_find_program_address(
    &[VAULT_AUTHORITY_SEED, &[lane_id]],
    program_id,
)
.unwrap()
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("schema/test.schema"),
        r#"
seed CONFIG_SEED config
seed VAULT_AUTHORITY_SEED vault-authority
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    assert_eq!(
        profile.pda_derivations.get("config"),
        Some(&PinocchioPdaDerivation {
            name: "config".to_string(),
            params: vec!["program_id".to_string()],
            param_types: BTreeMap::from([("program_id".to_string(), "&Pubkey".to_string(),)]),
            local_key_derivations: BTreeMap::new(),
            seeds: vec![PinocchioPdaSeed {
                expr: "CONFIG_SEED".to_string(),
                literal: Some("config".to_string()),
            }],
            program_id: "program_id".to_string(),
            returns_tuple: true,
        })
    );
    assert_eq!(
        profile.pda_derivations.get("vault_authority"),
        Some(&PinocchioPdaDerivation {
            name: "vault_authority".to_string(),
            params: vec!["program_id".to_string(), "lane_id".to_string()],
            param_types: BTreeMap::from([
                ("program_id".to_string(), "&Pubkey".to_string()),
                ("lane_id".to_string(), "u8".to_string()),
            ]),
            local_key_derivations: BTreeMap::new(),
            seeds: vec![
                PinocchioPdaSeed {
                    expr: "VAULT_AUTHORITY_SEED".to_string(),
                    literal: Some("vault-authority".to_string()),
                },
                PinocchioPdaSeed {
                    expr: "[lane_id]".to_string(),
                    literal: None,
                },
            ],
            program_id: "program_id".to_string(),
            returns_tuple: true,
        })
    );
}

#[test]
fn infers_account_key_derivation_from_syn_require_key_direct_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        r#"
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
pinocchio::pubkey::find_program_address(
    &[authority.as_ref(), mint.as_ref()],
    &ASSOCIATED_TOKEN_PROGRAM_ID,
)
}

fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
let [authority, mint, vault, ..] = accounts else {
    return Ok(());
};
require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let route = profile.handler("route").expect("route profile");
    assert_eq!(route.accounts, ["authority", "mint", "vault"]);
    assert_eq!(
        route.account_key_derivations.get("vault"),
        Some(&PinocchioLocalKeyDerivation {
            derivation: "token_vault".to_string(),
            args: vec!["authority.key()".to_string(), "mint.key()".to_string()],
        })
    );
    assert_eq!(
        profile.pda_derivations.get("token_vault"),
        Some(&PinocchioPdaDerivation {
            name: "token_vault".to_string(),
            params: vec!["authority".to_string(), "mint".to_string()],
            param_types: BTreeMap::from([
                ("authority".to_string(), "&Pubkey".to_string()),
                ("mint".to_string(), "&Pubkey".to_string()),
            ]),
            local_key_derivations: BTreeMap::new(),
            seeds: vec![
                PinocchioPdaSeed {
                    expr: "authority.as_ref()".to_string(),
                    literal: None,
                },
                PinocchioPdaSeed {
                    expr: "mint.as_ref()".to_string(),
                    literal: None,
                },
            ],
            program_id: "ASSOCIATED_TOKEN_PROGRAM_ID".to_string(),
            returns_tuple: true,
        })
    );
}

#[test]
fn context_inference_keeps_source_account_key_derivations() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace.path().join("program");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::write(
        program_root.join("src/lib.rs"),
        r#"
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
pinocchio::pubkey::find_program_address(
    &[authority.as_ref(), mint.as_ref()],
    &ASSOCIATED_TOKEN_PROGRAM_ID,
)
}

fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
let [authority, mint, vault, ..] = accounts else {
    return Ok(());
};
require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
Ok(())
}
"#,
    )
    .unwrap();
    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(&spec_path, "spec Program\n").unwrap();
    let output_src = program_root.join("src");

    let profile = infer_from_context(&output_src, Some(&spec_path)).expect("profile");
    let route = profile.handler("route").expect("route profile");
    assert!(route.account_key_derivations.contains_key("vault"));
    assert!(profile.pda_derivations.contains_key("token_vault"));
}

#[test]
fn parses_line_oriented_abi_schema_records_and_offsets() {
    let schema = parse_abi_schema(
        r#"
limit MAX_STEPS 3
instruction TRANSFER 7
account TRANSFER AUTHORITY 2
account TRANSFER DESTINATION 1
account TRANSFER SOURCE 0

record STEP
field LANE_ID u8
field AMOUNT u64
end

record TRANSFER_ARGS
field LANE_ID u8
field AMOUNT u64
repeat STEP step MAX_STEPS LANE_ID
end

record VAULT_ACCOUNT
field MAGIC bytes8
field OWNER pubkey
field BALANCE u64
end

record CUSTOM_STATE
field FLAG bool
end

magic VAULT_MAGIC VAULTMAG
instruction_record TRANSFER TRANSFER_ARGS
account_record CUSTOM CUSTOM_STATE
"#,
    );

    assert_eq!(schema.instructions.get("TRANSFER"), Some(&7));
    assert_eq!(
        schema.accounts.get("TRANSFER").unwrap(),
        &[
            IndexedName {
                name: "SOURCE".to_string(),
                index: 0,
                role: PinocchioAccountRole::default()
            },
            IndexedName {
                name: "DESTINATION".to_string(),
                index: 1,
                role: PinocchioAccountRole::default()
            },
            IndexedName {
                name: "AUTHORITY".to_string(),
                index: 2,
                role: PinocchioAccountRole::default()
            }
        ]
    );

    let args = schema.records.get("TRANSFER_ARGS").unwrap();
    assert_eq!(
        args.fields,
        [
            AbiField {
                name: "LANE_ID".to_string(),
                ty: "u8".to_string(),
                offset: 0,
                len: 1
            },
            AbiField {
                name: "AMOUNT".to_string(),
                ty: "u64".to_string(),
                offset: 1,
                len: 8
            }
        ]
    );
    assert_eq!(
        args.repeats,
        [AbiRepeat {
            name: "STEP".to_string(),
            ty: "step".to_string(),
            max_count: "MAX_STEPS".to_string(),
            count_field: "LANE_ID".to_string(),
            offset: 9,
            item_len: 9
        }]
    );
    assert_eq!(args.len, 36);
    assert_eq!(
        schema.account_layouts(),
        BTreeMap::from([
            ("custom".to_string(), "CUSTOM_STATE".to_string()),
            ("vault".to_string(), "VAULT_ACCOUNT".to_string())
        ])
    );
}

#[test]
fn abi_schema_exposes_repeated_record_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(dir.path().join("schema")).unwrap();
    std::fs::write(src.join("lib.rs"), "").unwrap();
    std::fs::write(
        dir.path().join("schema/test.schema"),
        r#"
limit MAX_TRANSFERS 4
instruction MOVE_BATCH 5

record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
field AMOUNT u64
end

record MOVE_BATCH_ARGS
field TRANSFER_COUNT u8
repeat TRANSFER transfer MAX_TRANSFERS TRANSFER_COUNT
end

record VAULT_ACCOUNT
field MAGIC bytes8
field OWNER pubkey
field BALANCE u64
end

magic VAULT_MAGIC VAULTMAG
instruction_record MOVE_BATCH MOVE_BATCH_ARGS
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let handler = profile.handler("move_batch").expect("move batch profile");

    assert!(handler.params.is_empty());
    assert_eq!(
        handler.repeats,
        [PinocchioRepeatField {
            name: "transfer".to_string(),
            count_field: "transfer_count".to_string(),
            offset: 1,
            item_len: 10,
            item_fields: vec![
                PinocchioParamField {
                    name: "from_lane_id".to_string(),
                    rust_type: "u8".to_string(),
                    start: 0,
                    end: 1
                },
                PinocchioParamField {
                    name: "to_lane_id".to_string(),
                    rust_type: "u8".to_string(),
                    start: 1,
                    end: 2
                },
                PinocchioParamField {
                    name: "amount".to_string(),
                    rust_type: "u64".to_string(),
                    start: 2,
                    end: 10
                }
            ]
        }]
    );
    assert_eq!(
        profile.account_layouts.get("vault"),
        Some(&"vault_account".to_string())
    );
    assert_eq!(
        profile.record_layouts.get("vault_account"),
        Some(&PinocchioRecordLayout {
            name: "vault_account".to_string(),
            len: 48,
            fields: vec![
                PinocchioLayoutField {
                    name: "magic".to_string(),
                    ty: "bytes8".to_string(),
                    offset: 0,
                    len: 8,
                    fixed_bytes: Some(b"VAULTMAG".to_vec()),
                },
                PinocchioLayoutField {
                    name: "owner".to_string(),
                    ty: "pubkey".to_string(),
                    offset: 8,
                    len: 32,
                    fixed_bytes: None,
                },
                PinocchioLayoutField {
                    name: "balance".to_string(),
                    ty: "u64".to_string(),
                    offset: 40,
                    len: 8,
                    fixed_bytes: None,
                }
            ],
            repeats: Vec::new(),
        })
    );
}

#[test]
fn abi_schema_fills_profile_tag_accounts_and_param_widths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(dir.path().join("schema")).unwrap();
    std::fs::write(src.join("lib.rs"), "").unwrap();
    std::fs::write(
        dir.path().join("schema/test.schema"),
        r#"
instruction TRANSFER 7
account TRANSFER AUTHORITY 2 signer
account TRANSFER DESTINATION 1 writable type token
account TRANSFER SOURCE 0 writable type token
account TRANSFER TOKEN_PROGRAM 3 program type token

record TRANSFER_ARGS
field LANE_ID u8
field AMOUNT u64
field MEMO bytes8
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(&src).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");

    assert_eq!(transfer.instruction_tag, Some(7));
    assert_eq!(
        transfer.accounts,
        ["source", "destination", "authority", "token_program"]
    );
    assert_eq!(
        transfer.account_roles.get("source"),
        Some(&PinocchioAccountRole {
            is_writable: Some(true),
            account_type: Some("token".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.account_roles.get("authority"),
        Some(&PinocchioAccountRole {
            is_signer: Some(true),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.account_roles.get("token_program"),
        Some(&PinocchioAccountRole {
            is_program: Some(true),
            account_type: Some("token".to_string()),
            ..PinocchioAccountRole::default()
        })
    );
    assert_eq!(
        transfer.params,
        [
            PinocchioParamField {
                name: "lane_id".to_string(),
                rust_type: "u8".to_string(),
                start: 0,
                end: 1
            },
            PinocchioParamField {
                name: "amount".to_string(),
                rust_type: "u64".to_string(),
                start: 1,
                end: 9
            },
            PinocchioParamField {
                name: "memo".to_string(),
                rust_type: "unsupported:bytes8".to_string(),
                start: 9,
                end: 17
            }
        ]
    );
}

#[test]
fn handler_lookup_falls_back_to_numeric_arity_base() {
    let mut handlers = BTreeMap::new();
    handlers.insert(
        "batch".to_string(),
        PinocchioHandlerProfile {
            name: "batch".to_string(),
            instruction_tag: Some(4),
            ..Default::default()
        },
    );
    let profile = PinocchioProofProfile {
        handlers,
        pda_derivations: BTreeMap::new(),
        record_layouts: BTreeMap::new(),
        account_layouts: BTreeMap::new(),
    };
    assert_eq!(
        profile.handler("batch_16").unwrap().instruction_tag,
        Some(4)
    );
}

#[test]
fn context_profile_prefers_spec_schema_over_generated_output() {
    let output_dir = tempfile::tempdir().expect("output tempdir");
    let output_src = output_dir.path().join("programs/src");
    std::fs::create_dir_all(&output_src).unwrap();
    std::fs::write(
        output_src.join("lib.rs"),
        r#"
pub fn process_instruction(
_program_id: &pinocchio::pubkey::Pubkey,
accounts: &[AccountInfo],
instruction_data: &[u8],
) -> ProgramResult {
let (discriminant, data) = instruction_data.split_first().unwrap();
match *discriminant {
    3 => instructions::transfer::process_transfer(accounts, data),
    _ => Err(ProgramError::InvalidInstructionData),
}
}
"#,
    )
    .unwrap();

    let workspace_dir = tempfile::tempdir().expect("workspace tempdir");
    let program_root = workspace_dir.path().join("program");
    let abi_root = workspace_dir.path().join("program-abi");
    std::fs::create_dir_all(program_root.join("src")).unwrap();
    std::fs::create_dir_all(program_root.join("verification")).unwrap();
    std::fs::create_dir_all(abi_root.join("schema")).unwrap();
    std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
    std::fs::write(
        abi_root.join("schema/program.schema"),
        r#"
instruction TRANSFER 1
account TRANSFER SOURCE 0
account TRANSFER DESTINATION 1

record TRANSFER_ARGS
field AMOUNT u64
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
    )
    .unwrap();
    let spec_path = program_root.join("verification/program.qedspec");
    std::fs::write(&spec_path, "spec Program\n").unwrap();

    let profile = infer_from_context(&output_src, Some(&spec_path)).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");

    assert_eq!(transfer.instruction_tag, Some(1));
    assert_eq!(transfer.accounts, ["source", "destination"]);
    assert_eq!(
        transfer.params,
        [PinocchioParamField {
            name: "amount".to_string(),
            rust_type: "u64".to_string(),
            start: 0,
            end: 8
        }]
    );
}

#[test]
fn infers_verified_stubs_from_contracted_helper_calls() {
    let dir = tempfile::tempdir().expect("src tempdir");
    let src = dir.path();
    std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
    std::fs::write(
        src.join("validation.rs"),
        r#"
#[cfg_attr(kani, kani::requires(amount > 0))]
#[cfg_attr(kani, kani::ensures(|result| result.is_ok()))]
pub fn check_amount_inner(amount: u64) -> Result<(), ()> {
if amount == 0 { Err(()) } else { Ok(()) }
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
check_amount_inner(amount)
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("processor.rs"),
        r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
check_amount(amount)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(src).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");

    assert_eq!(
        transfer.verified_stubs,
        ["crate::validation::check_amount_inner".to_string()]
    );
}

#[test]
fn infers_verified_stubs_from_contracted_bool_guard_helpers() {
    let dir = tempfile::tempdir().expect("src tempdir");
    let src = dir.path();
    std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
    std::fs::write(
        src.join("validation.rs"),
        r#"
#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_in_envelope_kani(amount: u64) -> bool {
amount <= 100
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
#[cfg(kani)]
{
    if !amount_in_envelope_kani(amount) {
        return Err(());
    }
    return Ok(());
}

#[cfg(not(kani))]
{
    if amount <= 100 { Ok(()) } else { Err(()) }
}
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("processor.rs"),
        r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
check_amount(amount)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(src).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");

    assert_eq!(
        transfer.verified_stubs,
        ["crate::validation::amount_in_envelope_kani".to_string()]
    );
}

#[test]
fn infers_verified_stubs_from_cfg_kani_and_not_kani_helper_branches() {
    let dir = tempfile::tempdir().expect("src tempdir");
    let src = dir.path();
    std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
    std::fs::write(
        src.join("validation.rs"),
        r#"
#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_in_envelope_kani(amount: u64) -> bool {
amount <= 100
}

#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_threshold_kani(amount: u64) -> bool {
amount <= 100
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
#[cfg(kani)]
{
    if !amount_in_envelope_kani(amount) {
        return Err(());
    }
    return Ok(());
}

#[cfg(not(kani))]
{
    if !amount_threshold_kani(amount) {
        return Err(());
    }
    Ok(())
}
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("processor.rs"),
        r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
check_amount(amount)?;
Ok(())
}
"#,
    )
    .unwrap();

    let profile = infer_from_src_dir(src).expect("profile");
    let transfer = profile.handler("transfer").expect("transfer profile");

    assert_eq!(
        transfer.verified_stubs,
        [
            "crate::validation::amount_in_envelope_kani".to_string(),
            "crate::validation::amount_threshold_kani".to_string(),
        ]
    );
}
