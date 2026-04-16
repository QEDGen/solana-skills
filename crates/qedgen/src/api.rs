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

const SYSTEM_PROMPT: &str =
    include_str!("../../../templates/prompts/system_prompt.txt");

const SBPF_SYSTEM_PROMPT: &str =
    include_str!("../../../templates/prompts/sbpf_system_prompt.txt");

const SBPF_SORRY_FILL_SYSTEM_PROMPT: &str =
    include_str!("../../../templates/prompts/sbpf_sorry_fill_prompt.txt");

/// Check if a prompt or code references sBPF types
fn is_sbpf_content(content: &str) -> bool {
    content.contains("QEDGen.Solana.SBPF")
        || content.contains("execute_step")
        || content.contains("Program := #[")
        || content.contains("initState")
        || (content.contains(".ldx") && content.contains(".exit"))
}

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
pub struct QedgenMetadata {
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

async fn call_mistral_api_with_system(
    client: &Client,
    prompt: &str,
    api_key: &str,
    temperature: f64,
    max_tokens: usize,
    system_prompt: &str,
) -> Result<(String, f64, Usage, String)> {
    let request = ChatRequest {
        model: MODEL.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
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
                    anyhow::bail!(
                        "Invalid or missing MISTRAL_API_KEY. Get one at https://console.mistral.ai"
                    );
                } else if status.as_u16() == 403 {
                    let error_body = resp.text().await.unwrap_or_default();
                    if error_body.contains("labs_not_enabled") {
                        anyhow::bail!("The Leanstral Labs model is not enabled for this Mistral organization.\nAsk an org admin to enable Labs models at https://admin.mistral.ai/plateforme/privacy and retry.");
                    } else {
                        anyhow::bail!("HTTP 403: {}", error_body);
                    }
                } else {
                    let error_body = resp.text().await.unwrap_or_default();
                    if status.is_server_error() {
                        // 5xx: transient server error, retry with backoff
                        eprintln!("ERROR: HTTP {} (retryable): {}", status, error_body);
                        if attempt < MAX_RETRIES - 1 {
                            sleep(Duration::from_millis(BACKOFF_BASE_MS * 2_u64.pow(attempt)))
                                .await;
                            continue;
                        }
                    } else {
                        // 4xx (other than 429/401/403): client error, don't retry
                        eprintln!("ERROR: HTTP {}: {}", status, error_body);
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

async fn call_mistral_api(
    client: &Client,
    prompt: &str,
    api_key: &str,
    temperature: f64,
    max_tokens: usize,
) -> Result<(String, f64, Usage, String)> {
    let system_prompt = if is_sbpf_content(prompt) {
        SBPF_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    call_mistral_api_with_system(
        client,
        prompt,
        api_key,
        temperature,
        max_tokens,
        system_prompt,
    )
    .await
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
                if let Some((existing_idx, _existing_text, existing_has_impl)) =
                    declarations.get(&name)
                {
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
        let meaningful_content = after_by
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("--"))
            .collect::<Vec<_>>();
        meaningful_content.is_empty()
    } else {
        false
    }
}

fn normalize_lean_code(code: &str) -> String {
    // sBPF proofs use QEDGen.Solana.SBPF, not Mathlib — skip Mathlib injection
    if is_sbpf_content(code) {
        return code.to_string();
    }

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
    // Always add Mathlib.Tactic import for tactics like split_ifs
    if !saw_mathlib_import {
        import_block.push("import Mathlib.Tactic");
    } else {
        import_block.push("import Mathlib");
    }
    import_block.extend(normalized_imports);

    let trimmed_body = body_lines.join("\n").trim_start().to_string();
    format!("{}\n\n{}\n", import_block.join("\n"), trimmed_body)
        .trim_end()
        .to_string()
        + "\n"
}

fn count_sorry(code: &str) -> usize {
    let re = regex::Regex::new(r"\bsorry\b").unwrap();
    re.find_iter(code).count()
}

#[allow(clippy::too_many_arguments)]
pub async fn generate_proofs(
    prompt: &str,
    output_dir: &Path,
    passes: usize,
    temperature: f64,
    max_tokens: usize,
    validate: bool,
    validation_workspace: Option<&Path>,
    mathlib: bool,
) -> Result<()> {
    let api_key = std::env::var("MISTRAL_API_KEY")
        .context("MISTRAL_API_KEY environment variable not set.\nGet a free key at https://console.mistral.ai\nThen run: export MISTRAL_API_KEY=your_key_here")?;

    // Create output directories
    std::fs::create_dir_all(output_dir)?;
    let attempts_dir = output_dir.join("attempts");
    std::fs::create_dir_all(&attempts_dir)?;

    // Set up Lean project files
    crate::project::setup_lean_project(output_dir, mathlib)?;

    // Save the prompt
    std::fs::write(output_dir.join("prompt.txt"), prompt)?;

    eprintln!(
        "Calling Leanstral model ({}) with pass@{}...",
        MODEL, passes
    );

    let client = Client::new();
    let mut metadata = QedgenMetadata {
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
        std::fs::write(
            attempts_dir.join(format!("completion_{}.lean", i)),
            &lean_code,
        )?;

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
            let candidate_lean = std::fs::read_to_string(
                attempts_dir.join(format!("completion_{}.lean", candidate.index)),
            )?;
            std::fs::write(output_dir.join("Best.lean"), &candidate_lean)?;

            eprint!(
                "  Validate completion_{}.lean ({} sorry)... ",
                candidate.index, candidate.sorry_count
            );
            let validation = crate::validate::validate_completion(
                output_dir,
                candidate.index,
                validation_workspace,
                mathlib,
            )
            .await?;

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

const SORRY_FILL_SYSTEM_PROMPT: &str =
    include_str!("../../../templates/prompts/sorry_fill_prompt.txt");

/// Parse sorry locations from a Lean file
fn find_sorry_locations(code: &str) -> Vec<(usize, String)> {
    let mut locations = Vec::new();
    let sorry_re = regex::Regex::new(r"\bsorry\b").unwrap();

    // Find enclosing theorem for each sorry
    let theorem_re =
        regex::Regex::new(r"(?m)^(theorem|lemma)\s+([a-zA-Z_][a-zA-Z0-9_']*)").unwrap();

    for mat in sorry_re.find_iter(code) {
        let line_num = code[..mat.start()].matches('\n').count() + 1;

        // Find the enclosing theorem
        let before = &code[..mat.start()];
        let enclosing = theorem_re
            .captures_iter(before)
            .last()
            .and_then(|c| c.get(2))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        locations.push((line_num, enclosing));
    }

    locations
}

/// Fill sorry markers in a Lean file using Leanstral
pub async fn fill_sorry(
    file_path: &Path,
    output_path: Option<&Path>,
    passes: usize,
    temperature: f64,
    max_tokens: usize,
    validate: bool,
) -> Result<()> {
    let api_key = std::env::var("MISTRAL_API_KEY")
        .context("MISTRAL_API_KEY environment variable not set.\nGet a free key at https://console.mistral.ai")?;

    let code = std::fs::read_to_string(file_path)
        .context(format!("Cannot read file: {}", file_path.display()))?;

    let sorry_locations = find_sorry_locations(&code);
    if sorry_locations.is_empty() {
        eprintln!("No sorry markers found in {}", file_path.display());
        return Ok(());
    }

    eprintln!(
        "Found {} sorry marker(s) in {}:",
        sorry_locations.len(),
        file_path.display()
    );
    for (line, theorem) in &sorry_locations {
        eprintln!("  line {}: in {}", line, theorem);
    }

    let prompt = format!(
        "Fill all `sorry` placeholders in this Lean 4 file with valid proofs.\n\n```lean4\n{}\n```",
        code
    );

    let client = Client::new();
    let sorry_system_prompt = if is_sbpf_content(&code) {
        SBPF_SORRY_FILL_SYSTEM_PROMPT
    } else {
        SORRY_FILL_SYSTEM_PROMPT
    };
    eprintln!(
        "\nCalling Leanstral model ({}) with pass@{}...",
        MODEL, passes
    );

    let mut best_code: Option<String> = None;
    let mut best_sorry_count = sorry_locations.len();

    for i in 0..passes {
        eprint!("  Pass {}/{}... ", i + 1, passes);
        let result = call_mistral_api_with_system(
            &client,
            &prompt,
            &api_key,
            temperature,
            max_tokens,
            sorry_system_prompt,
        )
        .await;

        match result {
            Ok((content, elapsed, usage, _)) => {
                let filled = extract_lean_code(&content);
                let sorry_count = count_sorry(&filled);
                eprintln!(
                    "done ({:.1}s, {} tokens, {} sorry remaining)",
                    elapsed, usage.completion_tokens, sorry_count
                );

                if sorry_count < best_sorry_count {
                    best_sorry_count = sorry_count;
                    best_code = Some(filled);
                }
                if sorry_count == 0 {
                    break;
                }
            }
            Err(e) => {
                eprintln!("error: {}", e);
            }
        }
    }

    let output = output_path.unwrap_or(file_path);

    if let Some(filled_code) = best_code {
        std::fs::write(output, &filled_code)?;
        eprintln!(
            "\nWrote filled proof to {} ({} sorry remaining)",
            output.display(),
            best_sorry_count
        );

        if validate {
            let project_dir = output.parent().unwrap_or(Path::new("."));
            eprintln!("Validating with lake build...");
            let status = std::process::Command::new("lake")
                .arg("build")
                .current_dir(project_dir)
                .status();
            match status {
                Ok(s) if s.success() => eprintln!("Validation: Success"),
                Ok(_) => eprintln!("Validation: Failed (see errors above)"),
                Err(e) => eprintln!("Validation: Could not run lake: {}", e),
            }
        }
    } else {
        eprintln!("\nNo improvement found. Original file unchanged.");
    }

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
