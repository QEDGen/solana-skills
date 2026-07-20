//! Runtime signer/init journey (#294 P0 item 2).
//!
//! PDA seeds, bump, payer, space, and `init` constraints are generated
//! by codegen and are invisible to every other gate: `cargo check`
//! compiles them, the spec-model harnesses never execute them, and the
//! snapshot suites only prove the text is stable. They are wrong only
//! at runtime. This journey executes real transactions against the
//! generated program under Mollusk.
//!
//! It lives in `qedgen-sandbox` rather than next to the other codegen
//! gates because the `qedgen` CLI crate deliberately does not depend on
//! Mollusk + Agave + the Solana SDK (see this crate's manifest); this is
//! the crate that already owns that dependency.
//!
//! The program is regenerated from the committed `.qedspec` on every
//! run, so the journey always tests CURRENT codegen output. Only the
//! handler bodies are committed — they are the agent-fill half that
//! codegen deliberately leaves as `todo!()`.
//!
//! Two journeys, covering both halves of the #294 requirement:
//!
//! - `pda_init_journey_and_constraint_violations` — the
//!   `Uninitialized -> Active` PDA initialization plus its failure
//!   modes (wrong seeds, non-canonical bump, unsigned payer, re-init).
//! - `token_cpi_journey_with_pda_authority` — an SPL Token CPI whose
//!   authority is the vault PDA, so the program signs with the vault's
//!   seeds and bump. Those must agree with the generated
//!   `#[account(seeds = …, bump)]` constraint; a mismatch compiles
//!   cleanly and fails only at runtime.
//!
//! `#[ignore]` because they shell out to `cargo build-sbf` (~30s warm,
//! minutes cold) and need the Solana platform tools.

use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

/// Must match the `declare_id!` the journey stamps into the generated
/// program: an Anchor `seeds` constraint validates against the running
/// program's own id, so the two cannot diverge.
const PROGRAM_ID: &str = "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS";

/// `8 (Anchor discriminator) + owner:32 + total:8 + bump:1 + status:1`.
/// Asserting the exact figure is what makes a wrong `space =` a failure
/// rather than a silent over-allocation.
const EXPECTED_ACCOUNT_LEN: usize = 8 + 32 + 8 + 1 + 1;

/// `Status::Active` — second variant of `type Vault | Uninitialized | Active`.
const STATUS_ACTIVE: u8 = 1;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qedgen-sandbox at <repo>/crates/qedgen-sandbox")
        .to_path_buf()
}

fn run_ok(command: &mut Command) -> String {
    let out = command.output().expect("spawn");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "command {:?} failed ({}):\n{combined}",
        command.get_program(),
        out.status
    );
    combined
}

/// Anchor's instruction discriminator: the first 8 bytes of
/// `sha256("global:<name>")`. `sha256_hex16` is exactly those 8 bytes in
/// hex, so the repo's own hashing crate supplies it.
fn discriminator(handler: &str) -> Vec<u8> {
    let hex = qedgen_hash_core::sha256_hex16(&format!("global:{handler}"));
    (0..8)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex byte"))
        .collect()
}

/// Regenerate the fixture program from its spec, overlay the committed
/// handler bodies, and build it to SBF bytecode. Returns the directory
/// holding `runtimevault.so`.
fn build_fixture_program() -> PathBuf {
    let root = repo_root();
    let fixture = root.join("crates/qedgen/tests/fixtures/runtime-journey");
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();

    std::fs::copy(fixture.join("vault.qedspec"), dir.join("vault.qedspec")).expect("copy spec");
    std::fs::create_dir(dir.join(".qed")).expect("create .qed");
    run_ok(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(dir),
    );

    let qedgen = root.join("target/debug/qedgen");
    assert!(
        qedgen.is_file(),
        "build the CLI first: cargo build --bin qedgen ({} missing)",
        qedgen.display()
    );
    run_ok(
        Command::new(&qedgen)
            .arg("codegen")
            .arg("--spec")
            .arg(dir.join("vault.qedspec"))
            .arg("--target")
            .arg("anchor")
            .arg("--output-dir")
            .arg(dir.join("programs"))
            .current_dir(dir),
    );

    // Overlay the agent-fill half. Codegen scaffolds these once and
    // never overwrites them, so committing only the bodies keeps every
    // generated file current.
    for handler in ["open", "deposit", "withdraw"] {
        std::fs::copy(
            fixture.join(format!("handlers/{handler}.rs")),
            dir.join(format!("programs/src/instructions/{handler}.rs")),
        )
        .unwrap_or_else(|e| panic!("overlay {handler}: {e}"));
    }

    // Codegen stamps the System Program id as a `declare_id!`
    // placeholder, which cannot be the program's own address at
    // runtime. Stamp the journey's id instead.
    let lib_path = dir.join("programs/src/lib.rs");
    let lib = std::fs::read_to_string(&lib_path).expect("read lib.rs");
    let stamped: String = lib
        .lines()
        .map(|line| {
            if line.starts_with("declare_id!") {
                format!("declare_id!(\"{PROGRAM_ID}\");")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(lib, stamped, "expected a declare_id! line to stamp");
    std::fs::write(&lib_path, stamped).expect("write lib.rs");

    // The generated manifest pins `qedgen-macros` to an unreleased tag.
    let manifest_path = dir.join("programs/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let macros = root.join("crates/qedgen-macros");
    let patched: String = manifest
        .lines()
        .map(|line| {
            if line.starts_with("qedgen-macros = {") {
                format!("qedgen-macros = {{ path = {macros:?} }}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest_path, patched).expect("write manifest");

    run_ok(
        Command::new("cargo")
            .arg("build-sbf")
            .current_dir(dir.join("programs")),
    );

    let deploy = dir.join("programs/target/deploy");
    assert!(
        deploy.join("runtimevault.so").is_file(),
        "cargo build-sbf produced no runtimevault.so in {}",
        deploy.display()
    );

    // Keep the build alive past the TempDir guard.
    let kept = std::env::temp_dir().join("qedgen-runtime-journey-deploy");
    let _ = std::fs::remove_dir_all(&kept);
    std::fs::create_dir_all(&kept).expect("create deploy dir");
    std::fs::copy(deploy.join("runtimevault.so"), kept.join("runtimevault.so")).expect("stage .so");
    kept
}

struct Journey {
    mollusk: Mollusk,
    program_id: Pubkey,
}

impl Journey {
    fn new(deploy_dir: &Path) -> Self {
        std::env::set_var("SBF_OUT_DIR", deploy_dir);
        let program_id = Pubkey::from_str(PROGRAM_ID).expect("program id");
        let mut mollusk = Mollusk::new(&program_id, "runtimevault");
        // The vault CPIs into SPL Token; without the real program in the
        // cache the transfer cannot execute at all.
        mollusk_svm_programs_token::token::add_program(&mut mollusk);
        Self {
            mollusk,
            program_id,
        }
    }

    fn vault_for(&self, owner: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"vault", owner.as_ref()], &self.program_id)
    }

    /// Build an `open` instruction. `owner_signs` and the `vault`
    /// address are parameterized so the negative controls can violate
    /// exactly one constraint at a time.
    fn open_ix(&self, owner: &Pubkey, vault: &Pubkey, owner_signs: bool) -> Instruction {
        let (system_id, _) = keyed_account_for_system_program();
        Instruction::new_with_bytes(
            self.program_id,
            &discriminator("open"),
            vec![
                AccountMeta::new(*owner, owner_signs),
                AccountMeta::new(*vault, false),
                AccountMeta::new_readonly(system_id, false),
            ],
        )
    }

    fn accounts(
        &self,
        owner: &Pubkey,
        vault: &Pubkey,
        vault_account: Account,
    ) -> Vec<(Pubkey, Account)> {
        let (system_id, system_account) = keyed_account_for_system_program();
        vec![
            (*owner, Account::new(10_000_000_000, 0, &system_id)),
            (*vault, vault_account),
            (system_id, system_account),
        ]
    }

    /// Open a vault and return its post-state account. Every token
    /// journey starts from a real, program-created vault rather than a
    /// hand-built one, so the bump under test is the one `init`
    /// actually stored.
    fn open_vault(&self, owner: &Pubkey) -> (Pubkey, Account) {
        let (vault, _) = self.vault_for(owner);
        let result = self.mollusk.process_instruction(
            &self.open_ix(owner, &vault, true),
            &self.accounts(owner, &vault, Account::default()),
        );
        assert!(
            result.program_result.is_ok(),
            "open must succeed before a token journey: {:?}",
            result.program_result
        );
        let account = result
            .resulting_accounts
            .iter()
            .find(|(k, _)| k == &vault)
            .map(|(_, a)| a.clone())
            .expect("vault in result");
        (vault, account)
    }

    /// `deposit` / `withdraw` share an account layout: owner, vault,
    /// vault_ta, owner_ta, token_program.
    fn token_ix(&self, handler: &str, ctx: &TokenCtx, amount: u64) -> Instruction {
        let mut data = discriminator(handler);
        data.extend_from_slice(&amount.to_le_bytes());
        Instruction::new_with_bytes(
            self.program_id,
            &data,
            vec![
                AccountMeta::new(ctx.owner, true),
                AccountMeta::new(ctx.vault, false),
                AccountMeta::new(ctx.vault_ta, false),
                AccountMeta::new(ctx.owner_ta, false),
                AccountMeta::new_readonly(mollusk_svm_programs_token::token::ID, false),
            ],
        )
    }
}

/// The account set a token journey threads between instructions.
struct TokenCtx {
    owner: Pubkey,
    vault: Pubkey,
    vault_ta: Pubkey,
    owner_ta: Pubkey,
    accounts: Vec<(Pubkey, Account)>,
}

impl TokenCtx {
    /// Build a mint plus two token accounts: `owner_ta` holding
    /// `owner_balance`, and `vault_ta` whose AUTHORITY IS THE VAULT PDA
    /// — the arrangement that makes `withdraw` a PDA-signed CPI.
    fn new(owner: Pubkey, vault: Pubkey, vault_account: Account, owner_balance: u64) -> Self {
        use mollusk_svm_programs_token::token;
        use spl_token_interface::state::{Account as TokenAccount, AccountState, Mint};

        let mint = Pubkey::new_unique();
        let owner_ta = Pubkey::new_unique();
        let vault_ta = Pubkey::new_unique();

        let mint_account = token::create_account_for_mint(Mint {
            mint_authority: None.into(),
            supply: owner_balance,
            decimals: 0,
            is_initialized: true,
            freeze_authority: None.into(),
        });
        let token_account = |authority: Pubkey, amount: u64| {
            token::create_account_for_token_account(TokenAccount {
                mint,
                owner: authority,
                amount,
                delegate: None.into(),
                state: AccountState::Initialized,
                is_native: None.into(),
                delegated_amount: 0,
                close_authority: None.into(),
            })
        };

        let (system_id, _) = keyed_account_for_system_program();
        let accounts = vec![
            (owner, Account::new(10_000_000_000, 0, &system_id)),
            (vault, vault_account),
            (vault_ta, token_account(vault, 0)),
            (owner_ta, token_account(owner, owner_balance)),
            token::keyed_account(),
            (mint, mint_account),
        ];

        Self {
            owner,
            vault,
            vault_ta,
            owner_ta,
            accounts,
        }
    }

    /// Carry an instruction's post-state forward, so a journey observes
    /// the same account evolution a real transaction sequence would.
    fn advance(&mut self, resulting: &[(Pubkey, Account)]) {
        for (key, account) in resulting {
            if let Some(slot) = self.accounts.iter_mut().find(|(k, _)| k == key) {
                slot.1 = account.clone();
            }
        }
    }

    fn token_amount(&self, key: &Pubkey) -> u64 {
        use solana_program_pack::Pack;
        let account = self
            .accounts
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, a)| a)
            .expect("token account present");
        spl_token_interface::state::Account::unpack(&account.data)
            .expect("token account decodes")
            .amount
    }

    fn vault_total(&self) -> u64 {
        let account = self
            .accounts
            .iter()
            .find(|(k, _)| k == &self.vault)
            .map(|(_, a)| a)
            .expect("vault present");
        u64::from_le_bytes(account.data[40..48].try_into().unwrap())
    }
}

#[test]
#[ignore = "shells out to cargo build-sbf; needs Solana platform tools"]
fn pda_init_journey_and_constraint_violations() {
    let deploy = build_fixture_program();
    let journey = Journey::new(&deploy);

    let owner = Pubkey::new_unique();
    let (vault, bump) = journey.vault_for(&owner);

    // ---- Success path: Uninitialized -> Active ----------------------
    let ix = journey.open_ix(&owner, &vault, true);
    let accounts = journey.accounts(&owner, &vault, Account::default());
    let result = journey.mollusk.process_instruction(&ix, &accounts);

    assert!(
        result.program_result.is_ok(),
        "open must succeed with canonical seeds, bump, and a signing payer: {:?}",
        result.program_result
    );

    let vault_post = result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == vault)
        .map(|(_, a)| a.clone())
        .expect("vault account in result");

    // `init` really created a program-owned account...
    assert_eq!(
        vault_post.owner, journey.program_id,
        "init must transfer ownership to the program"
    );
    // ...at exactly the space codegen asked for...
    assert_eq!(
        vault_post.data.len(),
        EXPECTED_ACCOUNT_LEN,
        "space = 8 + INIT_SPACE must allocate exactly the state layout"
    );
    assert!(
        vault_post.lamports > 0,
        "init must fund the account via payer"
    );

    // ...and the handler wrote the state the spec describes.
    let data = &vault_post.data;
    assert_eq!(
        &data[8..40],
        owner.as_ref(),
        "owner field must be the payer"
    );
    assert_eq!(
        u64::from_le_bytes(data[40..48].try_into().unwrap()),
        0,
        "total starts at zero"
    );
    assert_eq!(
        data[48], bump,
        "stored bump must be the canonical bump Anchor derived"
    );
    assert_eq!(
        data[49], STATUS_ACTIVE,
        "lifecycle must advance Uninitialized -> Active"
    );

    // ---- Negative control: wrong seeds ------------------------------
    // A PDA derived from a different seed is not the account the
    // generated `seeds = [b"vault", owner]` constraint expects.
    let (wrong_seed_vault, _) =
        Pubkey::find_program_address(&[b"vau1t", owner.as_ref()], &journey.program_id);
    let ix = journey.open_ix(&owner, &wrong_seed_vault, true);
    let accounts = journey.accounts(&owner, &wrong_seed_vault, Account::default());
    assert!(
        journey
            .mollusk
            .process_instruction(&ix, &accounts)
            .program_result
            .is_err(),
        "open must reject a PDA derived from the wrong seeds"
    );

    // ---- Negative control: non-canonical address (bump) -------------
    // `bump` with no value pins the CANONICAL bump. Any other valid
    // program address for these seeds must be rejected.
    let mut off_curve = None;
    for candidate_bump in (0..bump).rev() {
        if let Ok(addr) = Pubkey::create_program_address(
            &[b"vault", owner.as_ref(), &[candidate_bump]],
            &journey.program_id,
        ) {
            off_curve = Some(addr);
            break;
        }
    }
    let non_canonical = off_curve.expect("a non-canonical bump should yield a valid address");
    assert_ne!(
        non_canonical, vault,
        "sanity: distinct from the canonical PDA"
    );
    let ix = journey.open_ix(&owner, &non_canonical, true);
    let accounts = journey.accounts(&owner, &non_canonical, Account::default());
    assert!(
        journey
            .mollusk
            .process_instruction(&ix, &accounts)
            .program_result
            .is_err(),
        "open must reject a non-canonical bump"
    );

    // ---- Negative control: payer does not sign ----------------------
    let fresh_owner = Pubkey::new_unique();
    let (fresh_vault, _) = journey.vault_for(&fresh_owner);
    let ix = journey.open_ix(&fresh_owner, &fresh_vault, false);
    let accounts = journey.accounts(&fresh_owner, &fresh_vault, Account::default());
    assert!(
        journey
            .mollusk
            .process_instruction(&ix, &accounts)
            .program_result
            .is_err(),
        "open must reject when the payer does not sign"
    );

    // ---- Negative control: re-initialization ------------------------
    // Feed the post-state back in: `init` must refuse an account the
    // program already owns.
    let ix = journey.open_ix(&owner, &vault, true);
    let accounts = journey.accounts(&owner, &vault, vault_post);
    assert!(
        journey
            .mollusk
            .process_instruction(&ix, &accounts)
            .program_result
            .is_err(),
        "open must reject re-initialization of an existing vault"
    );
}

/// The other half of #294's P0 item 2: an SPL Token CPI whose authority
/// is a program PDA.
///
/// `deposit` moves tokens with the OWNER as authority — an ordinary
/// signed CPI. `withdraw` moves them back with the VAULT PDA as
/// authority, so the program must sign the CPI with the vault's seeds
/// and bump. Those seeds have to agree with the generated
/// `#[account(seeds = …, bump)]` constraint; a mismatch compiles
/// cleanly and fails only here.
#[test]
#[ignore = "shells out to cargo build-sbf; needs Solana platform tools"]
fn token_cpi_journey_with_pda_authority() {
    const STARTING_BALANCE: u64 = 1_000;
    const DEPOSIT: u64 = 400;
    const WITHDRAW: u64 = 150;

    let deploy = build_fixture_program();
    let journey = Journey::new(&deploy);

    let owner = Pubkey::new_unique();
    let (vault, vault_account) = journey.open_vault(&owner);
    let mut ctx = TokenCtx::new(owner, vault, vault_account, STARTING_BALANCE);

    // ---- Owner-authorized CPI: deposit ------------------------------
    let result = journey
        .mollusk
        .process_instruction(&journey.token_ix("deposit", &ctx, DEPOSIT), &ctx.accounts);
    assert!(
        result.program_result.is_ok(),
        "deposit must succeed with the owner as token authority: {:?}",
        result.program_result
    );
    ctx.advance(&result.resulting_accounts);

    assert_eq!(
        ctx.token_amount(&ctx.owner_ta),
        STARTING_BALANCE - DEPOSIT,
        "deposit must debit the owner's token account"
    );
    assert_eq!(
        ctx.token_amount(&ctx.vault_ta),
        DEPOSIT,
        "deposit must credit the vault's token account"
    );
    assert_eq!(
        ctx.vault_total(),
        DEPOSIT,
        "deposit must track the balance in vault state"
    );

    // ---- PDA-authorized CPI: withdraw -------------------------------
    // The vault PDA is the token authority here, so this only succeeds
    // if the signer seeds the handler passes match the seeds and bump
    // on the generated account constraint.
    let result = journey
        .mollusk
        .process_instruction(&journey.token_ix("withdraw", &ctx, WITHDRAW), &ctx.accounts);
    assert!(
        result.program_result.is_ok(),
        "withdraw must succeed: the CPI signer seeds must match the \
         generated seeds/bump constraint: {:?}",
        result.program_result
    );
    ctx.advance(&result.resulting_accounts);

    assert_eq!(
        ctx.token_amount(&ctx.vault_ta),
        DEPOSIT - WITHDRAW,
        "withdraw must debit the vault's token account via the PDA-signed CPI"
    );
    assert_eq!(
        ctx.token_amount(&ctx.owner_ta),
        STARTING_BALANCE - DEPOSIT + WITHDRAW,
        "withdraw must credit the owner's token account"
    );
    assert_eq!(
        ctx.vault_total(),
        DEPOSIT - WITHDRAW,
        "withdraw must track the balance in vault state"
    );

    // ---- Guard still holds over the CPI path ------------------------
    // `requires total >= amount`: overdrawing must reject rather than
    // attempt a CPI that would fail deeper in the token program.
    let result = journey.mollusk.process_instruction(
        &journey.token_ix("withdraw", &ctx, DEPOSIT * 10),
        &ctx.accounts,
    );
    assert!(
        result.program_result.is_err(),
        "withdraw must reject an amount exceeding the tracked total"
    );
}
