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

Produce a single Lean 4 module that is as likely as possible to compile under Lean 4.15 + Mathlib 4.15.

Hard requirements:
1. Output exactly one Lean module.
2. Do not emit duplicate declarations.
3. Do not leave theorem bodies empty after `:= by`.
4. Do not invent identifiers, namespaces, or APIs that are not defined in the file or imported from Lean/Mathlib.
5. Use only Lean 4 / Mathlib identifiers you are confident exist in this toolchain version.
6. If a proof is incomplete, use `sorry` inside the proof body rather than leaving a stub.
7. Prefer a smaller, explicit semantic model over an ambitious but broken one.
8. Define the state transition functions before proving theorems about them.
9. If several properties are requested, it is acceptable to prove a smaller subset well rather than all of them badly.

Recommended structure:
- imports
- model types
- instruction/state transition functions
- helper lemmas
- theorems

Output policy:
- Prefer plain Lean code only.
- Do not include prose before or after the code unless explicitly requested.
- Use `import Mathlib` only if needed.
- Prefer self-contained proofs and simple executable definitions.
"#;

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
        extracted.join("\n\n")
    } else {
        content.to_string()
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
        assert!(result.contains("\n\n")); // Blocks should be joined with double newline
    }
}
