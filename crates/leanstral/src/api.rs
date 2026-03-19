use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::time::sleep;

const API_URL: &str = "https://api.mistral.ai/v1/chat/completions";
const MODEL: &str = "labs-leanstral-2603";
const TIMEOUT_SECS: u64 = 180;
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_MS: u64 = 2000;

const SYSTEM_PROMPT: &str = r#"You are Leanstral, an expert Lean 4 proof engineer.

GOAL: Produce a single Lean 4 module that COMPILES under Lean 4.15 + Mathlib 4.15 with <5% sorry usage.

CRITICAL SYNTAX RULE - Parameter Naming Convention:
ALL function parameters MUST be prefixed with `p_` to avoid conflicts with Lean reserved keywords.

Examples:
WRONG: `def transfer (from to amount : Nat) := ...`
RIGHT: `def transfer (p_from p_to p_amount : Nat) := ...`

WRONG: `def withdraw (s amt : Nat) := ...`
RIGHT: `def withdraw (p_s p_amt : Nat) := ...`

WRONG: `def initialize := ...`
RIGHT: `def initEscrow := ...`  (for function names, use descriptive names like initEscrow, createEscrow, setupEscrow)

Always use this pattern:
- Function parameters: `p_from`, `p_to`, `p_state`, `p_amount`, `p_key`
- Function names: `initEscrow`, `cancelEscrow`, `executeExchange` (avoid `initialize`)
- Structure fields: `isClosed`, `isActive`, `balance`, `authority` (no prefix needed for fields)
- Local variables in proofs: can use any name, but `p_` prefix is safe if unsure

Hard constraints:
1. Output exactly one Lean module in a SINGLE ```lean4 code block
2. Do not output prose, explanations, headers, or multiple code blocks
3. Each theorem/function/type appears exactly once
4. Every theorem must include a complete proof body
5. Aim for 0 `sorry`; absolute maximum is <5% of theorems using `sorry`
6. Define every helper function, structure, theorem, and constant before first use
7. Use only identifiers and constants that are defined in the file or are standard in Lean 4.15 / Mathlib 4.15
8. Do NOT invent library constants or theorem names such as `Nat.le_max`, `Nat.add_le_max`, `u64_max`, etc. If you need a bound, define it explicitly in the file
9. Do NOT use `_` placeholders in theorem statements or definitions when Lean must infer a witness or value
10. In particular, do NOT write propositions like `f x = some _`
11. Instead, use an explicit existential: `∃ y, f x = some y`
12. Do NOT use `.get!` in theorem statements
13. Avoid `.get!` in proofs unless a prior hypothesis has already rewritten the expression to `some v`
14. Prefer theorem statements of the form `f s = some s' -> ...` or `∃ s', f s = some s' ∧ ...`
15. Prefer simple, executable definitions and simple theorem statements that are actually provable
16. If a property is too strong for the chosen model, simplify the model or theorem statement so it compiles and proves cleanly
17. Prefer `simp`, `rfl`, `constructor`, `aesop`, `omega`, `cases`, `rcases`, and explicit rewriting over brittle tactics
18. Do not call tactics that do not match the goal shape; for example, do not use `split_ifs` unless an `if` expression is present in the goal or local hypotheses
19. Avoid dependent pattern matching unless necessary
20. Prefer total functions returning `Option` for state transitions when success/failure matters

Modeling guidance:
- Prefer a small abstract model over a realistic but fragile one
- Use `Nat` for balances and account identifiers unless a stronger type is truly needed
- For arithmetic safety, if you need a `u64` bound, define it explicitly, for example:
  `def U64_MAX : Nat := 2^64 - 1`
- State transitions should be simple and deterministic
- If proving full token conservation is difficult, define a simple total-balance helper and prove conservation by simplification
- If proving successful execution properties, make success explicit with hypotheses like `h : exchange s taker = some s'`

Recommended structure:
- imports
- constants
- structures / inductives
- helper functions
- transition functions
- helper lemmas
- final theorems

Pre-output checklist:
- No Lean keywords used as identifiers
- No `_` placeholders in theorem statements
- No `.get!` in theorem statements
- No invented constants or theorem names
- Every referenced name is defined or imported
- Every theorem is provable with the chosen model
- Output is exactly one `lean4` code block

Example patterns you should imitate:

Example 1: simple structure and definitional proof with p_ prefix
```lean4
import Mathlib

structure State where
  balance : Nat

def credit (p_s : State) (p_amt : Nat) : State :=
  { p_s with balance := p_s.balance + p_amt }

theorem credit_balance (p_s : State) (p_amt : Nat) :
    (credit p_s p_amt).balance = p_s.balance + p_amt := by
  simp [credit]
```

Example 2: `Option` transition with explicit success witness and p_ prefix
```lean4
import Mathlib

structure State where
  balance : Nat

def withdraw (p_s : State) (p_amt : Nat) : Option State :=
  if h : p_amt <= p_s.balance then
    some { p_s with balance := p_s.balance - p_amt }
  else
    none

theorem withdraw_success_balance (p_s p_s' : State) (p_amt : Nat)
    (h : withdraw p_s p_amt = some p_s') :
    p_s'.balance = p_s.balance - p_amt := by
  simp [withdraw] at h
  split_ifs at h with hle
  · cases h
    simp
  · contradiction
```

Example 3: success stated with an existential, not `some _`, with p_ prefix
```lean4
import Mathlib

structure State where
  openFlag : Bool

def closeState (p_s : State) : Option State :=
  some { p_s with openFlag := false }

theorem closeState_exists (p_s : State) :
    ∃ s', closeState p_s = some s' := by
  refine ⟨{ p_s with openFlag := false }, ?_⟩
  simp [closeState]
```

Example 4: record update pattern inside a larger state with p_ prefix
```lean4
import Mathlib

structure Escrow where
  isClosed : Bool

structure ProgramState where
  escrow : Escrow
  counter : Nat

def markClosed (p_s : ProgramState) : ProgramState :=
  { p_s with escrow := { p_s.escrow with isClosed := true } }

theorem markClosed_closed (p_s : ProgramState) :
    (markClosed p_s).escrow.isClosed = true := by
  simp [markClosed]
```

Example 5: conjunction proof with `constructor` and p_ prefix
```lean4
import Mathlib

theorem pairFacts (p_a p_b : Nat) :
    p_a = p_a ∧ p_b = p_b := by
  constructor
  · rfl
  · rfl
```

Example 6: explicit arithmetic bound with a defined constant and p_ prefix
```lean4
import Mathlib

def U64_MAX : Nat := 2^64 - 1

theorem bounded_add_safe (p_x p_y : Nat)
    (hx : p_x <= U64_MAX) (hy : p_y <= U64_MAX) (hxy : p_x + p_y <= U64_MAX) :
    p_x + p_y <= U64_MAX := by
  omega
```

Output requirements:
- Output plain Lean code only, inside a single ```lean4 code block
- Do NOT include any explanation before or after the code block
- Do NOT emit duplicate declarations
- Do NOT emit theorem stubs followed later by proofs
- If a theorem is too difficult, simplify the theorem statement before writing the proof
- Prefer compilable, modest theorems over ambitious but broken ones

When in doubt, choose the simplest model and the simplest theorem statement that still captures the requested property and compiles cleanly."#;

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
    max_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageContent,
    finish_reason: String,
}

#[derive(Debug, Deserialize)]
struct ChatMessageContent {
    content: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionMetadata {
    pub index: usize,
    pub sorry_count: usize,
    pub elapsed_seconds: f64,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub finish_reason: String,
    pub build_status: BuildStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_log_path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    NotRun,
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LeanstralMetadata {
    pub model: String,
    pub passes: usize,
    pub temperature: f64,
    pub max_tokens: usize,
    pub validate: bool,
    pub completions: Vec<CompletionMetadata>,
    pub best_completion_index: usize,
    pub best_sorry_count: usize,
    pub best_selection_reason: String,
}

async fn call_mistral_api(
    client: &Client,
    prompt: &str,
    api_key: &str,
    temperature: f64,
    max_tokens: usize,
) -> Result<(String, f64, Usage, String)> {
    let request = ChatRequest {
        model: MODEL.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            },
        ],
        temperature,
        max_tokens,
    };

    for attempt in 0..MAX_RETRIES {
        let start = Instant::now();
        let response = client
            .post(API_URL)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request)
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .send()
            .await;

        let elapsed = start.elapsed().as_secs_f64();

        match response {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let body: ChatResponse = resp.json().await?;
                    let content = body
                        .choices
                        .first()
                        .context("No choices in response")?
                        .message
                        .content
                        .clone();
                    let finish_reason = body
                        .choices
                        .first()
                        .context("No choices in response")?
                        .finish_reason
                        .clone();
                    return Ok((content, elapsed, body.usage, finish_reason));
                } else if status.as_u16() == 429 {
                    let wait = BACKOFF_BASE_MS * 2_u64.pow(attempt);
                    eprintln!(
                        "  Rate limited (429). Retrying in {}s... (attempt {}/{})",
                        wait / 1000,
                        attempt + 1,
                        MAX_RETRIES
                    );
                    sleep(Duration::from_millis(wait)).await;
                    continue;
                } else if status.as_u16() == 401 {
                    anyhow::bail!("Invalid or missing MISTRAL_API_KEY. Get one at https://console.mistral.ai");
                } else if status.as_u16() == 403 {
                    let error_body = resp.text().await.unwrap_or_default();
                    if error_body.contains("labs_not_enabled") {
                        anyhow::bail!("The Leanstral Labs model is not enabled for this Mistral organization.\nAsk an org admin to enable Labs models at https://admin.mistral.ai/plateforme/privacy and retry.");
                    } else {
                        anyhow::bail!("HTTP 403: {}", error_body);
                    }
                } else {
                    let error_body = resp.text().await.unwrap_or_default();
                    eprintln!("ERROR: HTTP {}: {}", status, error_body);
                    if attempt < MAX_RETRIES - 1 {
                        sleep(Duration::from_millis(BACKOFF_BASE_MS * 2_u64.pow(attempt))).await;
                        continue;
                    }
                    anyhow::bail!("HTTP {}: {}", status, error_body);
                }
            }
            Err(e) => {
                eprintln!("ERROR: {}", e);
                if attempt < MAX_RETRIES - 1 {
                    sleep(Duration::from_millis(BACKOFF_BASE_MS * 2_u64.pow(attempt))).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }

    anyhow::bail!("All retries exhausted")
}

fn extract_lean_code(content: &str) -> String {
    // Extract code from ```lean or ```lean4 blocks
    // (?s) enables dotall mode so . matches newlines
    let re = regex::Regex::new(r"(?s)```lean4?\s*\n(.*?)```").unwrap();
    let mut extracted = Vec::new();

    for cap in re.captures_iter(content) {
        if let Some(code) = cap.get(1) {
            extracted.push(code.as_str());
        }
    }

    if !extracted.is_empty() {
        // If we have multiple blocks, try to deduplicate them
        if extracted.len() > 1 {
            deduplicate_lean_blocks(&extracted)
        } else {
            extracted[0].to_string()
        }
    } else {
        content.to_string()
    }
}

fn deduplicate_lean_blocks(blocks: &[&str]) -> String {
    use std::collections::{HashMap, HashSet};

    // Parse each block to find declarations (theorem, def, structure, inductive, etc.)
    let decl_pattern = regex::Regex::new(
        r"(?m)^(theorem|def|structure|inductive|class|instance|axiom|lemma)\s+([a-zA-Z_][a-zA-Z0-9_']*)"
    ).unwrap();

    // Map declaration names to their best implementation
    let mut declarations: HashMap<String, (usize, &str, bool)> = HashMap::new();
    let mut imports = Vec::new();
    let mut seen_imports = HashSet::new();

    for (block_idx, block) in blocks.iter().enumerate() {
        // Collect imports from all blocks
        for line in block.lines() {
            if line.trim().starts_with("import ") {
                let import_stmt = line.trim();
                if !seen_imports.contains(import_stmt) {
                    imports.push(import_stmt);
                    seen_imports.insert(import_stmt);
                }
            }
        }

        // Find all declarations in this block
        for cap in decl_pattern.captures_iter(block) {
            if let (Some(_kind), Some(name_match)) = (cap.get(1), cap.get(2)) {
                let name = name_match.as_str().to_string();
                let decl_start = cap.get(0).unwrap().start();

                // Find the end of this declaration (next declaration or end of block)
                let next_decl = decl_pattern.find_at(block, decl_start + 1);
                let decl_end = next_decl.map(|m| m.start()).unwrap_or(block.len());
                let decl_text = &block[decl_start..decl_end];

                // Determine if this has a real implementation
                // A stub typically has `:= by` followed by nothing or just whitespace
                let has_implementation = !is_stub(decl_text);

                // Keep the declaration with implementation, or the latest one if both are stubs
                if let Some((existing_idx, _existing_text, existing_has_impl)) = declarations.get(&name) {
                    // Prefer the one with implementation
                    if has_implementation && !existing_has_impl {
                        declarations.insert(name, (block_idx, decl_text, has_implementation));
                    } else if !has_implementation && *existing_has_impl {
                        // Keep existing
                    } else {
                        // Both have impl or both are stubs, keep the later one
                        if block_idx > *existing_idx {
                            declarations.insert(name, (block_idx, decl_text, has_implementation));
                        }
                    }
                } else {
                    declarations.insert(name, (block_idx, decl_text, has_implementation));
                }
            }
        }
    }

    // If deduplication didn't help much, just join blocks
    if declarations.is_empty() {
        return blocks.join("\n\n");
    }

    // Reconstruct the code with deduplicated declarations
    let mut result = String::new();

    // Add imports first
    if !imports.is_empty() {
        result.push_str(&imports.join("\n"));
        result.push_str("\n\n");
    }

    // Add all declarations in a reasonable order (by block index, then position)
    let mut sorted_decls: Vec<_> = declarations.values().collect();
    sorted_decls.sort_by_key(|(block_idx, _, _)| *block_idx);

    for (_, decl_text, _) in sorted_decls {
        result.push_str(decl_text);
        result.push_str("\n\n");
    }

    result.trim_end().to_string()
}

fn is_stub(decl_text: &str) -> bool {
    // Check if this is a stub declaration (has := by but no proof body)
    if let Some(by_pos) = decl_text.find(":= by") {
        let after_by = &decl_text[by_pos + 5..].trim();
        // If there's nothing after `:= by` or just whitespace/comments, it's a stub
        let meaningful_content = after_by.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("--"))
            .collect::<Vec<_>>();
        meaningful_content.is_empty()
    } else {
        false
    }
}

fn normalize_lean_code(code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let mut normalized_imports = Vec::new();
    let mut body_lines = Vec::new();
    let mut saw_mathlib_import = false;

    let import_re = regex::Regex::new(r"^import\s+Mathlib(\..+)?\s*$").unwrap();
    let import_general_re = regex::Regex::new(r"^import\s+").unwrap();

    for line in lines {
        if import_re.is_match(line) {
            saw_mathlib_import = true;
            continue;
        }
        if import_general_re.is_match(line) {
            normalized_imports.push(line);
            continue;
        }
        body_lines.push(line);
    }

    let mut import_block = Vec::new();
    if saw_mathlib_import {
        import_block.push("import Mathlib");
    }
    import_block.extend(normalized_imports);

    if import_block.is_empty() {
        return code.to_string();
    }

    let trimmed_body = body_lines.join("\n").trim_start().to_string();
    format!("{}\n\n{}\n", import_block.join("\n"), trimmed_body).trim_end().to_string() + "\n"
}

fn count_sorry(code: &str) -> usize {
    let re = regex::Regex::new(r"\bsorry\b").unwrap();
    re.find_iter(code).count()
}

pub async fn generate_proofs(
    prompt: &str,
    output_dir: &Path,
    passes: usize,
    temperature: f64,
    max_tokens: usize,
    validate: bool,
    validation_workspace: Option<&Path>,
) -> Result<()> {
    let api_key = std::env::var("MISTRAL_API_KEY")
        .context("MISTRAL_API_KEY environment variable not set.\nGet a free key at https://console.mistral.ai\nThen run: export MISTRAL_API_KEY=your_key_here")?;

    // Create output directories
    std::fs::create_dir_all(output_dir)?;
    let attempts_dir = output_dir.join("attempts");
    std::fs::create_dir_all(&attempts_dir)?;

    // Set up Lean project files
    crate::project::setup_lean_project(output_dir)?;

    // Save the prompt
    std::fs::write(output_dir.join("prompt.txt"), prompt)?;

    eprintln!("Calling Leanstral ({}) with pass@{}...", MODEL, passes);

    let client = Client::new();
    let mut metadata = LeanstralMetadata {
        model: MODEL.to_string(),
        passes,
        temperature,
        max_tokens,
        validate,
        completions: Vec::new(),
        best_completion_index: 0,
        best_sorry_count: usize::MAX,
        best_selection_reason: "fewest_sorry".to_string(),
    };

    let mut best_idx = 0;
    let mut best_sorry_count = usize::MAX;

    for i in 0..passes {
        eprint!("  Pass {}/{}... ", i + 1, passes);
        let (content, elapsed, usage, finish_reason) =
            call_mistral_api(&client, prompt, &api_key, temperature, max_tokens).await?;

        let lean_code = normalize_lean_code(&extract_lean_code(&content));
        let sorry_count = count_sorry(&lean_code);

        eprintln!(
            "done ({:.1}s, {} tokens, {} sorry)",
            elapsed, usage.completion_tokens, sorry_count
        );

        // Save raw and extracted code
        std::fs::write(
            attempts_dir.join(format!("completion_{}_raw.txt", i)),
            &content,
        )?;
        std::fs::write(attempts_dir.join(format!("completion_{}.lean", i)), &lean_code)?;

        metadata.completions.push(CompletionMetadata {
            index: i,
            sorry_count,
            elapsed_seconds: elapsed,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            finish_reason,
            build_status: BuildStatus::NotRun,
            build_log_path: None,
        });

        if sorry_count < best_sorry_count {
            best_sorry_count = sorry_count;
            best_idx = i;
        }
    }

    if validate {
        eprintln!("\nValidating completions with 'lake build Best'...");
        let mut ranked_candidates = metadata.completions.clone();
        ranked_candidates.sort_by(|a, b| {
            if a.sorry_count != b.sorry_count {
                a.sorry_count.cmp(&b.sorry_count)
            } else {
                a.index.cmp(&b.index)
            }
        });

        let mut found_validated = false;
        for candidate in ranked_candidates {
            let candidate_lean =
                std::fs::read_to_string(attempts_dir.join(format!("completion_{}.lean", candidate.index)))?;
            std::fs::write(output_dir.join("Best.lean"), &candidate_lean)?;

            eprint!(
                "  Validate completion_{}.lean ({} sorry)... ",
                candidate.index, candidate.sorry_count
            );
            let validation = crate::validate::validate_completion(output_dir, candidate.index, validation_workspace).await?;

            // Update metadata
            let meta = metadata
                .completions
                .iter_mut()
                .find(|m| m.index == candidate.index)
                .unwrap();
            meta.build_status = validation.status;
            meta.build_log_path = validation.log_path;

            eprintln!("{:?}", validation.status);

            if validation.status == BuildStatus::Success {
                best_idx = candidate.index;
                best_sorry_count = candidate.sorry_count;
                metadata.best_selection_reason = "validated_build".to_string();
                found_validated = true;
                break;
            }
        }

        if !found_validated {
            metadata.best_selection_reason = "fewest_sorry_no_valid_build".to_string();
        }
    }

    metadata.best_completion_index = best_idx;
    metadata.best_sorry_count = best_sorry_count;

    // Save metadata
    std::fs::write(
        output_dir.join("metadata.json"),
        serde_json::to_string_pretty(&metadata)?,
    )?;

    // Copy best completion to Best.lean
    let best_lean =
        std::fs::read_to_string(attempts_dir.join(format!("completion_{}.lean", best_idx)))?;
    std::fs::write(output_dir.join("Best.lean"), &best_lean)?;

    eprintln!("\nResults saved to {}/", output_dir.display());
    eprintln!(
        "Best completion: Best.lean (from attempts/completion_{}.lean, {} sorry)",
        best_idx, best_sorry_count
    );
    eprintln!("Selection reason: {}", metadata.best_selection_reason);
    eprintln!("\nTo verify the proof:");
    eprintln!("  cd {}", output_dir.display());
    eprintln!("  lake build   # Build and verify proofs");

    // Print best completion to stdout
    println!("{}", best_lean);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_lean_code_multiline() {
        let input = r#"Here is some Lean code:

```lean4
theorem add_comm (a b : Nat) : a + b = b + a := by
  induction a with
  | zero => simp
  | succ a ih => simp [ih]
```

That's the proof."#;

        let result = extract_lean_code(input);
        assert!(result.contains("theorem add_comm"));
        assert!(result.contains("induction a with"));
        assert!(result.contains("| zero => simp"));
        assert!(result.contains("| succ a ih => simp [ih]"));
    }

    #[test]
    fn test_extract_lean_code_single_line() {
        let input = r#"```lean
def id (x : Nat) := x
```"#;

        let result = extract_lean_code(input);
        assert_eq!(result.trim(), "def id (x : Nat) := x");
    }

    #[test]
    fn test_extract_lean_code_no_blocks() {
        let input = "Just some plain text without code blocks";
        let result = extract_lean_code(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_extract_lean_code_multiple_blocks() {
        let input = r#"First block:
```lean4
def foo := 1
```

Second block:
```lean
def bar := 2
```"#;

        let result = extract_lean_code(input);
        assert!(result.contains("def foo := 1"));
        assert!(result.contains("def bar := 2"));
    }

    #[test]
    fn test_deduplicate_theorem_stubs() {
        let input = r#"Here are the theorems:
```lean4
theorem add_comm (a b : Nat) : a + b = b + a := by
```

And here are the proofs:
```lean4
theorem add_comm (a b : Nat) : a + b = b + a := by
  omega
```"#;

        let result = extract_lean_code(input);
        // Should only contain one instance of add_comm
        let count = result.matches("theorem add_comm").count();
        assert_eq!(count, 1, "Should have exactly one add_comm theorem");
        // Should have the version with the proof body
        assert!(result.contains("omega"), "Should contain the proof body");
    }

    #[test]
    fn test_deduplicate_multiple_stubs_and_impls() {
        let input = r#"Types:
```lean4
structure Point where
  x : Nat
  y : Nat
```

Theorem stubs:
```lean4
theorem point_eq (p : Point) : p.x + p.y = p.y + p.x := by
```

Proofs:
```lean4
theorem point_eq (p : Point) : p.x + p.y = p.y + p.x := by
  omega
```"#;

        let result = extract_lean_code(input);
        let theorem_count = result.matches("theorem point_eq").count();
        assert_eq!(theorem_count, 1, "Should have exactly one point_eq theorem");
        let struct_count = result.matches("structure Point").count();
        assert_eq!(struct_count, 1, "Should have exactly one Point structure");
        assert!(result.contains("omega"));
    }
}
