use super::*;

fn write_project(tmp: &tempfile::TempDir, files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = tmp.path().to_path_buf();
    for (rel, contents) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    root
}

#[test]
fn adapt_renders_anchor_scaffold_program() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                pub mod instructions;

                #[program]
                pub mod my_escrow {
                    use super::*;
                    pub fn initialize(ctx: Context<Initialize>, deposit_amount: u64, receive_amount: u64) -> Result<()> {
                        instructions::initialize::handler(ctx, deposit_amount, receive_amount)
                    }
                    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
                        instructions::cancel::handler(ctx)
                    }
                }
                "#,
            ),
            (
                "src/instructions/mod.rs",
                "pub mod initialize;\npub mod cancel;\n",
            ),
            (
                "src/instructions/initialize.rs",
                r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Initialize>, deposit_amount: u64, receive_amount: u64) -> Result<()> {
                    Ok(())
                }
                "#,
            ),
            (
                "src/instructions/cancel.rs",
                r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Cancel>) -> Result<()> {
                    Ok(())
                }
                "#,
            ),
        ],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();

    assert!(
        rendered.contains("spec MyEscrow"),
        "rendered:\n{}",
        rendered
    );
    assert!(rendered.contains("handler initialize (deposit_amount : U64) (receive_amount : U64)"));
    assert!(rendered.contains("handler cancel : State.Init -> State.Init"));
    assert!(rendered.contains("src/instructions/initialize.rs"));
    assert!(rendered.contains("src/instructions/cancel.rs"));
    assert!(rendered.contains("accounts struct: `Initialize`"));
    assert!(rendered.contains("accounts struct: `Cancel`"));
    // Round-trip parsability is enforced inside `adapt()` itself.
}

#[test]
fn extract_program_model_captures_anchor_handlers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;
                pub mod instructions;

                #[program]
                pub mod my_escrow {
                    use super::*;
                    pub fn initialize(ctx: Context<Initialize>, amount: u64) -> Result<()> {
                        instructions::initialize::handler(ctx, amount)
                    }
                }
                "#,
            ),
            ("src/instructions/mod.rs", "pub mod initialize;\n"),
            (
                "src/instructions/initialize.rs",
                r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Initialize>, amount: u64) -> Result<()> {
                    Ok(())
                }
                "#,
            ),
        ],
    );

    let model = extract_program_model(&root, &HashMap::new()).unwrap();

    assert_eq!(model.framework, ProgramFramework::Anchor);
    assert_eq!(model.name, "my_escrow");
    assert_eq!(
        model.primary_source.as_deref(),
        Some(Path::new("src/lib.rs"))
    );
    assert_eq!(model.entry_module.as_deref(), Some("my_escrow"));
    assert_eq!(model.handlers.len(), 1);

    let handler = &model.handlers[0];
    assert_eq!(handler.name, "initialize");
    assert_eq!(handler.accounts_type.as_deref(), Some("Initialize"));
    assert_eq!(
        handler.source_path.as_deref(),
        Some(Path::new("src/instructions/initialize.rs"))
    );
    assert_eq!(handler.shape, HandlerShape::FreeFn);
    assert_eq!(handler.args.len(), 1);
    assert_eq!(handler.args[0].name, "amount");
    assert_eq!(handler.args[0].qedspec_type.as_deref(), Some("U64"));
}

#[test]
fn anchor_adapter_trait_detects_extracts_and_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod inline_prog {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
        )],
    );
    let overrides = HashMap::new();
    let adapter = AnchorAdapter::new(&overrides);

    assert_eq!(adapter.framework(), ProgramFramework::Anchor);
    assert!(adapter.detect(&root));

    let model = adapter.extract(&root).unwrap();
    assert_eq!(model.name, "inline_prog");
    let rendered = adapter.render_spec(&model).unwrap();
    assert!(rendered.contains("spec InlineProg"));
    assert!(rendered.contains("handler initialize (x : U64)"));
}

#[test]
fn adapt_handles_inline_handler_body() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod inline_prog {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                        require!(x > 0, ErrorCode::Bad);
                        ctx.accounts.state.x = x;
                        Ok(())
                    }
                }
                "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(rendered.contains("inline body in the `#[program]` mod"));
    assert!(rendered.contains("src/lib.rs"));
}

#[test]
fn adapt_marks_unrecognized_handlers_with_todo() {
    // Forwarder names a nonexistent free fn: classifier says FreeFn,
    // resolver fails, renderer marks UNRECOGNIZED; output must still parse.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn dispatch(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                        nowhere::missing(ctx, data)
                    }
                }
                "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(rendered.contains("UNRECOGNIZED"), "rendered:\n{}", rendered);
    assert!(rendered.contains("classify this handler manually"));
}

#[test]
fn adapt_emits_typed_arg_for_user_defined_struct() {
    // Bare-path type with no generics passes through by name.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn create(ctx: Context<Create>, args: CreateArgs) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(
        rendered.contains("(args : CreateArgs)"),
        "expected user-defined type passthrough, got:\n{}",
        rendered
    );
}

#[test]
fn adapt_falls_back_for_generic_arg_types() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn ingest(ctx: Context<Ingest>, payload: Vec<u8>) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    // U64 placeholder in the signature; explanatory TODO in the body.
    assert!(rendered.contains("(payload : U64)"));
    assert!(
        rendered.contains("could not map `payload` from Rust source"),
        "rendered:\n{}",
        rendered
    );
}

#[test]
fn adapt_to_file_writes_and_creates_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                #[program]
                pub mod tiny {
                    use super::*;
                    pub fn ping(ctx: Context<Ping>) -> Result<()> { Ok(()) }
                }
                "#,
        )],
    );

    let out = tmp.path().join("nested/out/tiny.qedspec");
    adapt_to_file(&root, &out, &HashMap::new()).unwrap();
    assert!(out.exists());
    let contents = std::fs::read_to_string(&out).unwrap();
    assert!(contents.contains("spec Tiny"));
    assert!(contents.contains("handler ping"));
}

/// Asserts `adapt(<repo>/<demo_rel>)` matches `<demo_rel>/before.qedspec`
/// byte-for-byte. Regenerate after intentional renderer changes:
///   cargo run -- adapt --program <demo_rel> --out <demo_rel>/before.qedspec
fn assert_snapshot(demo_rel: &str) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root must be two parents up from CARGO_MANIFEST_DIR");
    let demo = repo_root.join(demo_rel);
    let expected_path = demo.join("before.qedspec");

    let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
        panic!(
            "could not read snapshot at {}: {}\n\
                 (run `cargo run -- adapt --program {} --out {}` to create it)",
            expected_path.display(),
            e,
            demo_rel,
            expected_path.display(),
        )
    });

    let actual = adapt(&demo, &HashMap::new()).expect("adapter must succeed on the fixture");

    assert_eq!(
        actual,
        expected,
        "snapshot drift in {}/before.qedspec.\n\
             If intentional, regenerate with:\n\
             cargo run -- adapt --program {} --out {}",
        demo_rel,
        demo_rel,
        expected_path.display(),
    );
}

/// Anchor-scaffold style: free-fn forwarders into `instructions/<name>.rs`
/// (`FreeFn` classifier).
#[test]
fn adapt_matches_brownfield_demo_snapshot() {
    assert_snapshot("crates/qedgen/tests/fixtures/anchor-brownfield-demo");
}

/// Marinade style: `ctx.accounts.<method>(...)` forwarder
/// (`AccountsMethod` classifier + impl-method resolution).
#[test]
fn adapt_matches_marinade_style_snapshot() {
    assert_snapshot(
        "crates/qedgen/tests/fixtures/regressions/anchor-adapter-shapes/marinade-style",
    );
}

/// Squads V4 style: `<Type>::<method>(ctx, args)` forwarder (`TypeAssoc`
/// classifier; impls inline with the program mod, not a sibling file).
#[test]
fn adapt_matches_squads_style_snapshot() {
    assert_snapshot("crates/qedgen/tests/fixtures/regressions/anchor-adapter-shapes/squads-style");
}

#[test]
fn discovers_error_code_enum_with_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                    #[program]
                    pub mod p {
                        use super::*;
                        pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                    }
                    "#,
            ),
            (
                "src/errors.rs",
                r#"
                    use anchor_lang::prelude::*;

                    #[error_code]
                    pub enum ErrorCode {
                        #[msg("invalid")]
                        InvalidArgument,
                        #[msg("overflow")]
                        Overflow,
                        #[msg("not authorized")]
                        NotAuthorized,
                    }
                    "#,
            ),
        ],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(
        rendered.contains("`#[error_code] pub enum ErrorCode`"),
        "rendered:\n{}",
        rendered
    );
    assert!(
        rendered.contains("| InvalidArgument"),
        "rendered:\n{}",
        rendered
    );
    assert!(rendered.contains("| Overflow"));
    assert!(rendered.contains("| NotAuthorized"));
    assert!(!rendered.contains("(No `#[error_code]` enum found"));
}

#[test]
fn falls_back_to_placeholder_when_no_error_code_enum() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                }
                "#,
        )],
    );
    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(rendered.contains("(No `#[error_code]` enum found"));
    assert!(rendered.contains("| InvalidArgument"));
}

#[test]
fn handles_qualified_error_code_attribute() {
    // `#[anchor_lang::error_code]` matches via the last path segment.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                }

                #[anchor_lang::error_code]
                pub enum MyError {
                    Bad,
                }
                "#,
        )],
    );
    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(rendered.contains("`#[error_code] pub enum MyError`"));
    assert!(rendered.contains("| Bad"));
}

/// Method-shape handlers (`ctx.accounts.process(...)`) emit a sealed
/// `#[qed]` attribute via `body_hash_for_impl_fn`.
#[test]
fn compute_attributes_seals_method_shape_handlers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                    use anchor_lang::prelude::*;

                    pub mod instructions;

                    #[program]
                    pub mod stake {
                        use super::*;
                        pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
                            ctx.accounts.process(amount)
                        }
                    }

                    pub struct Deposit;
                    "#,
            ),
            ("src/instructions/mod.rs", "pub mod deposit;\n"),
            (
                "src/instructions/deposit.rs",
                r#"
                    use anchor_lang::prelude::*;
                    use crate::Deposit;

                    impl Deposit {
                        pub fn process(&mut self, amount: u64) -> Result<()> {
                            Ok(())
                        }
                    }
                    "#,
            ),
        ],
    );

    let spec_path = tmp.path().join("stake.qedspec");
    std::fs::write(
        &spec_path,
        r#"
            spec Stake
            type State | Active
            handler deposit (amount : U64) : State.Active -> State.Active {
              effect { lamports += amount }
            }
            type Error | Bad
            "#,
    )
    .unwrap();

    let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.handler, "deposit");
    assert!(
        e.note.is_none(),
        "method-shape should seal cleanly: {:?}",
        e.note
    );
    assert!(e.attribute.contains("hash = \""), "attr: {}", e.attribute);
    assert!(
        e.attribute.contains("spec_hash = \""),
        "attr: {}",
        e.attribute
    );
}

/// A found `Context<X>` struct adds the `accounts*` triplet so the macro
/// can seal the struct too.
#[test]
fn compute_attributes_includes_accounts_struct_seal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
                        Ok(())
                    }
                }

                #[derive(Accounts)]
                pub struct Buy<'info> {
                    pub buyer: Signer<'info>,
                    #[account(mut)]
                    pub vault: Account<'info, Vault>,
                }

                pub struct Vault;
                "#,
        )],
    );

    let spec_path = tmp.path().join("p.qedspec");
    std::fs::write(
        &spec_path,
        r#"
            spec P
            type State | Active
            handler buy (amount : U64) : State.Active -> State.Active {
              effect { count += amount }
            }
            type Error | Bad
            "#,
    )
    .unwrap();

    let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
    let buy = entries.iter().find(|e| e.handler == "buy").unwrap();
    assert!(
        buy.attribute.contains("accounts = \"Buy\""),
        "attr: {}",
        buy.attribute
    );
    assert!(buy.attribute.contains("accounts_file = \"src/lib.rs\""));
    assert!(buy.attribute.contains("accounts_hash = \""));
}

/// Without a resolvable `Context<X>` struct, the adapter falls back to
/// the body+spec-only attribute.
#[test]
fn compute_attributes_omits_accounts_when_struct_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn ping(ctx: Context<MissingType>) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
        )],
    );

    let spec_path = tmp.path().join("p.qedspec");
    std::fs::write(
        &spec_path,
        r#"
            spec P
            type State | Active
            handler ping : State.Active -> State.Active { effect { } }
            type Error | Bad
            "#,
    )
    .unwrap();

    let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
    let ping = entries.iter().find(|e| e.handler == "ping").unwrap();
    assert!(
        !ping.attribute.contains("accounts = "),
        "attr: {}",
        ping.attribute
    );
    assert!(ping.attribute.contains("hash = \""));
}

/// Two `pub struct Shared` in different modules + `Context<crate::b::Shared>`
/// MUST seal against `crate::b::Shared`, not the first ident match.
#[test]
fn compute_attributes_respects_qualified_accounts_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                    use anchor_lang::prelude::*;

                    pub mod a;
                    pub mod b;

                    #[program]
                    pub mod p {
                        use super::*;
                        pub fn act(ctx: Context<crate::b::Shared>, amount: u64) -> Result<()> {
                            Ok(())
                        }
                    }
                    "#,
            ),
            (
                "src/a.rs",
                r#"
                    use anchor_lang::prelude::*;

                    #[derive(Accounts)]
                    pub struct Shared<'info> {
                        pub user: Signer<'info>,
                        // a's version: just a signer.
                    }
                    "#,
            ),
            (
                "src/b.rs",
                r#"
                    use anchor_lang::prelude::*;

                    #[derive(Accounts)]
                    pub struct Shared<'info> {
                        #[account(mut)]
                        pub vault: Account<'info, Vault>,
                        pub authority: Signer<'info>,
                    }

                    pub struct Vault;
                    "#,
            ),
        ],
    );

    let spec_path = tmp.path().join("p.qedspec");
    std::fs::write(
        &spec_path,
        r#"
            spec P
            type State | Active
            handler act (amount : U64) : State.Active -> State.Active {
              effect { count += amount }
            }
            type Error | Bad
            "#,
    )
    .unwrap();

    let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
    let act = entries.iter().find(|e| e.handler == "act").unwrap();
    assert!(
        act.attribute.contains("accounts_file = \"src/b.rs\""),
        "qualified path `crate::b::Shared` should resolve to src/b.rs, got: {}",
        act.attribute
    );
    // And the hash MUST be the b.rs version, not the a.rs first-match.
    let b_hash = crate::spec_hash::accounts_struct_hash(
        &std::fs::read_to_string(root.join("src/b.rs")).unwrap(),
        "Shared",
    )
    .unwrap();
    assert!(
        act.attribute
            .contains(&format!("accounts_hash = \"{}\"", b_hash)),
        "expected hash from b.rs, got: {}",
        act.attribute
    );
}

#[test]
fn handler_override_parses_module_paths() {
    let p = HandlerOverride::parse("instructions::buy::handler").unwrap();
    assert_eq!(p.module_path, vec!["instructions", "buy"]);
    assert_eq!(p.fn_name, "handler");

    let bare = HandlerOverride::parse("handler").unwrap();
    assert!(bare.module_path.is_empty());
    assert_eq!(bare.fn_name, "handler");

    // Empty input or empty segments → None
    assert!(HandlerOverride::parse("").is_none());
    assert!(HandlerOverride::parse("instructions::buy::").is_none());
    assert!(HandlerOverride::parse("::handler").is_none());
}

#[test]
fn parse_handler_override_splits_on_first_equals() {
    let (name, parsed) = parse_handler_override("dispatch=instructions::dispatch::run").unwrap();
    assert_eq!(name, "dispatch");
    assert_eq!(parsed.module_path, vec!["instructions", "dispatch"]);
    assert_eq!(parsed.fn_name, "run");

    // Missing `=`, empty name, empty path: all errors
    assert!(parse_handler_override("dispatch").is_err());
    assert!(parse_handler_override("=path::fn").is_err());
    assert!(parse_handler_override("dispatch=").is_err());
}

#[test]
fn override_resolves_unrecognized_handler_to_free_fn() {
    // Custom-dispatcher shape the classifier can't follow; a `--handler`
    // override resolves it cleanly.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[
            (
                "src/lib.rs",
                r#"
                    use anchor_lang::prelude::*;

                    pub mod instructions;

                    #[program]
                    pub mod dispatcher {
                        use super::*;
                        pub fn dispatch(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                            // Custom dispatcher — classifier can't follow this.
                            DISPATCH_TABLE.lookup(data)(ctx, data)
                        }
                    }

                    pub struct Dispatch;
                    "#,
            ),
            ("src/instructions/mod.rs", "pub mod dispatch;\n"),
            (
                "src/instructions/dispatch.rs",
                r#"
                    use anchor_lang::prelude::*;
                    use crate::Dispatch;

                    pub fn handler(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                        Ok(())
                    }
                    "#,
            ),
        ],
    );

    let mut overrides = HashMap::new();
    overrides.insert(
        "dispatch".to_string(),
        HandlerOverride::parse("instructions::dispatch::handler").unwrap(),
    );

    let rendered = adapt(&root, &overrides).unwrap();
    assert!(
        !rendered.contains("UNRECOGNIZED"),
        "rendered:\n{}",
        rendered
    );
    assert!(rendered.contains("free-fn forwarder"));
    assert!(rendered.contains("src/instructions/dispatch.rs"));
}

#[test]
fn to_pascal_case_handles_snake_and_already_pascal() {
    assert_eq!(to_pascal_case("my_escrow"), "MyEscrow");
    assert_eq!(to_pascal_case("token_mill"), "TokenMill");
    assert_eq!(to_pascal_case("escrow"), "Escrow");
    assert_eq!(to_pascal_case("AlreadyPascal"), "AlreadyPascal");
}

#[test]
fn adapt_renders_accounts_block_from_derive_accounts() {
    // #257: the skeleton fills `accounts { }` mechanically from the handler's
    // `#[derive(Accounts)]` struct — signer / writable / program / typed —
    // instead of a bare `// TODO: accounts`. Covers Box + InterfaceAccount +
    // Program + read-only-typed classification that the snapshot fixtures don't.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
            use anchor_lang::prelude::*;

            #[program]
            pub mod bank {
                use super::*;
                pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
                    Ok(())
                }
            }

            #[derive(Accounts)]
            pub struct Deposit<'info> {
                #[account(mut)]
                pub authority: Signer<'info>,
                #[account(mut)]
                pub vault: Account<'info, Vault>,
                pub config: Box<Account<'info, Config>>,
                pub mint: InterfaceAccount<'info, Mint>,
                pub token_program: Program<'info, Token>,
            }

            #[account]
            pub struct Vault { pub total: u64 }
            #[account]
            pub struct Config { pub fee: u64 }
            "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();

    // The mechanically-derived block replaced the TODO.
    assert!(
        !rendered.contains("// TODO: accounts { ... }"),
        "accounts TODO should be gone:\n{}",
        rendered
    );
    assert!(rendered.contains("accounts {"), "rendered:\n{}", rendered);
    assert!(rendered.contains("authority : signer, writable"));
    assert!(rendered.contains("vault : writable"));
    assert!(
        rendered.contains("config : type Config"),
        "rendered:\n{}",
        rendered
    );
    assert!(rendered.contains("mint : type Mint"));
    assert!(rendered.contains("token_program : program"));
    // Lone signer surfaced as an auth hint, not a live (unbound) `auth` clause.
    assert!(rendered.contains("// TODO: auth authority — declared signer"));
    // adapt() enforces round-trip parseability internally, so a well-formed
    // accounts block is also proven to parse.
}

#[test]
fn adapt_accounts_respects_qualified_accounts_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/regressions/anchor-accounts-collision");

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    let lite = rendered
        .split("handler lite")
        .nth(1)
        .and_then(|rest| rest.split("handler heavy").next())
        .unwrap();
    let heavy = rendered.split("handler heavy").nth(1).unwrap();

    assert!(lite.contains("user : signer"), "rendered:\n{rendered}");
    assert!(!lite.contains("vault : writable"), "rendered:\n{rendered}");
    assert!(heavy.contains("vault : writable"), "rendered:\n{rendered}");
    assert!(
        heavy.contains("authority : signer"),
        "rendered:\n{rendered}"
    );
    assert!(!heavy.contains("user : signer"), "rendered:\n{rendered}");
}

#[test]
fn adapt_recognizes_account_signer_constraints() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
            use anchor_lang::prelude::*;

            #[program]
            pub mod constrained_signers {
                use super::*;
                pub fn act(ctx: Context<Act>) -> Result<()> { Ok(()) }
            }

            #[derive(Accounts)]
            pub struct Act<'info> {
                #[account(signer)]
                pub authority: UncheckedAccount<'info>,
                #[account(mut, signer)]
                pub payer: AccountInfo<'info>,
            }
            "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(
        rendered.contains("authority : signer"),
        "rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("payer : signer, writable"),
        "rendered:\n{rendered}"
    );
}

#[test]
fn derives_state_from_account_status_enum() {
    // #258: the skeleton seeds `type State` from an `#[account]` struct's
    // status-enum field, preferring a `status`/`state` field and the richest
    // enum, instead of the flat `Init | Active` placeholder.
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
            use anchor_lang::prelude::*;

            #[program]
            pub mod gov {
                use super::*;
                pub fn vote(ctx: Context<Vote>) -> Result<()> { Ok(()) }
            }

            #[derive(Accounts)]
            pub struct Vote<'info> {
                #[account(mut)]
                pub proposal: Account<'info, Proposal>,
                pub voter: Signer<'info>,
            }

            #[account]
            pub struct Proposal {
                pub status: ProposalStatus,
            }

            // A second, poorer enum candidate that must NOT win (fewer variants,
            // non-status field name).
            #[account]
            pub struct Config {
                pub mode: Mode,
            }

            pub enum ProposalStatus { Draft, Active, Approved, Executed, Cancelled }
            pub enum Mode { A, B }
            "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();

    assert!(
        rendered.contains("derived from `Proposal.status: ProposalStatus`"),
        "rendered:\n{}",
        rendered
    );
    for v in ["Draft", "Active", "Approved", "Executed", "Cancelled"] {
        assert!(
            rendered.contains(&format!("  | {}", v)),
            "missing {v}:\n{rendered}"
        );
    }
    // Init placeholder retained so `State.Init -> State.Init` transitions stay valid.
    assert!(rendered.contains("  | Init"));
    // The poorer `Mode` enum did not become the State.
    assert!(!rendered.contains("derived from `Config.mode"));
    // Flat placeholder replaced.
    assert!(!rendered.contains("// TODO: replace with the actual lifecycle"));
}

#[test]
fn falls_back_to_placeholder_state_when_no_status_enum() {
    let tmp = tempfile::tempdir().unwrap();
    let root = write_project(
        &tmp,
        &[(
            "src/lib.rs",
            r#"
            use anchor_lang::prelude::*;

            #[program]
            pub mod bank {
                use super::*;
                pub fn deposit(ctx: Context<Deposit>) -> Result<()> { Ok(()) }
            }

            #[derive(Accounts)]
            pub struct Deposit<'info> {
                pub authority: Signer<'info>,
            }

            #[account]
            pub struct Vault { pub total: u64 }
            "#,
        )],
    );

    let rendered = adapt(&root, &HashMap::new()).unwrap();
    assert!(
        rendered.contains("// TODO: replace with the actual lifecycle"),
        "rendered:\n{}",
        rendered
    );
    assert!(rendered.contains("  | Active"));
}
