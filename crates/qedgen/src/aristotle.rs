use anyhow::{Context, Result};
use reqwest::multipart;
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;

const BASE_URL: &str = "https://aristotle.harmonic.fun/api/v2";
const REQUEST_TIMEOUT_SECS: u64 = 60;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 30;
const MAX_POLL_FAILURE_SECS: u64 = 600;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Project {
    pub project_id: String,
    pub status: String,
    pub created_at: String,
    pub last_updated_at: String,
    pub percent_complete: Option<i32>,
    pub input_prompt: Option<String>,
    pub file_name: Option<String>,
    pub description: Option<String>,
    pub output_summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    projects: Vec<Project>,
    #[allow(dead_code)]
    pagination_key: Option<String>,
}

fn api_key() -> Result<String> {
    std::env::var("ARISTOTLE_API_KEY").context(
        "ARISTOTLE_API_KEY environment variable not set.\n\
         Get a key at https://aristotle.harmonic.fun",
    )
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .http1_only()
        .build()
        .expect("failed to build HTTP client")
}

/// Create a tar.gz archive of a Lean project directory, filtering out
/// build artifacts and large dependencies (same rules as aristotlelib).
fn tar_project_dir(dir: &Path) -> Result<Vec<u8>> {
    use std::io::Write;

    let skip_dirs: &[&str] = &[".git", ".lake"];
    let skip_extensions: &[&str] = &[
        "olean", "ilean", "ir", "o", "so", "a", "dylib", "trace",
    ];

    let mut tar_builder = tar::Builder::new(Vec::new());

    fn walk(
        base: &Path,
        current: &Path,
        tar: &mut tar::Builder<Vec<u8>>,
        skip_dirs: &[&str],
        skip_extensions: &[&str],
    ) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if path.is_dir() {
                if skip_dirs.iter().any(|&s| name_str == s) {
                    continue;
                }
                walk(base, &path, tar, skip_dirs, skip_extensions)?;
            } else {
                if let Some(ext) = path.extension() {
                    if skip_extensions.iter().any(|&s| ext == s) {
                        continue;
                    }
                }
                let rel = path.strip_prefix(base)?;
                tar.append_path_with_name(&path, rel)?;
            }
        }
        Ok(())
    }

    walk(dir, dir, &mut tar_builder, skip_dirs, skip_extensions)?;
    let tar_data = tar_builder.into_inner()?;

    // gzip the tar
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_data)?;
    Ok(encoder.finish()?)
}

/// Submit a Lean project directory to Aristotle for sorry-filling.
/// Returns the project ID for polling.
pub async fn submit(project_dir: &Path, prompt: &str) -> Result<Project> {
    let key = api_key()?;
    let client = client();

    eprintln!("Packaging project directory...");
    let tar_bytes = tar_project_dir(project_dir)?;
    eprintln!("  Archive size: {} KB", tar_bytes.len() / 1024);

    let body_json = serde_json::json!({ "prompt": prompt }).to_string();

    let file_part = multipart::Part::bytes(tar_bytes)
        .file_name("project.tar.gz")
        .mime_str("application/x-tar")?;

    let form = multipart::Form::new()
        .text("body", body_json)
        .part("input", file_part);

    eprintln!("Submitting to Aristotle...");
    let resp = client
        .post(format!("{}/project", BASE_URL))
        .header("X-API-Key", &key)
        .multipart(form)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Aristotle API error ({}): {}", status, body);
    }

    let project: Project = resp.json().await?;
    eprintln!(
        "Project created: {} (status: {})",
        project.project_id, project.status
    );
    Ok(project)
}

/// Poll a project until it reaches a terminal status.
/// Returns the final project state.
pub async fn poll(project_id: &str, interval_secs: Option<u64>) -> Result<Project> {
    let key = api_key()?;
    let client = client();
    let interval = Duration::from_secs(interval_secs.unwrap_or(DEFAULT_POLL_INTERVAL_SECS));
    let mut cumulative_failure = Duration::ZERO;

    loop {
        let resp = client
            .get(format!("{}/project/{}", BASE_URL, project_id))
            .header("X-API-Key", &key)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                cumulative_failure = Duration::ZERO;
                let project: Project = r.json().await?;

                let pct = project.percent_complete.unwrap_or(0);
                eprint!("\r  Status: {:<20} {:>3}%", project.status, pct);

                match project.status.as_str() {
                    "QUEUED" | "IN_PROGRESS" | "NOT_STARTED" => {
                        sleep(interval).await;
                    }
                    _ => {
                        eprintln!();
                        return Ok(project);
                    }
                }
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                let backoff = Duration::from_secs(15).min(
                    Duration::from_secs(120)
                        .min(cumulative_failure + Duration::from_secs(15)),
                );
                cumulative_failure += backoff;
                if cumulative_failure.as_secs() > MAX_POLL_FAILURE_SECS {
                    anyhow::bail!(
                        "Polling failed for over {} seconds. Last error ({}): {}",
                        MAX_POLL_FAILURE_SECS,
                        status,
                        body
                    );
                }
                eprintln!("\n  API error ({}), retrying in {}s...", status, backoff.as_secs());
                sleep(backoff).await;
            }
            Err(e) => {
                let backoff = Duration::from_secs(15);
                cumulative_failure += backoff;
                if cumulative_failure.as_secs() > MAX_POLL_FAILURE_SECS {
                    anyhow::bail!(
                        "Polling failed for over {} seconds. Last error: {}",
                        MAX_POLL_FAILURE_SECS,
                        e
                    );
                }
                eprintln!("\n  Network error, retrying in 15s: {}", e);
                sleep(backoff).await;
            }
        }
    }
}

/// Get the current status of a project (single request, no polling).
pub async fn status(project_id: &str) -> Result<Project> {
    let key = api_key()?;
    let client = client();

    let resp = client
        .get(format!("{}/project/{}", BASE_URL, project_id))
        .header("X-API-Key", &key)
        .send()
        .await?;

    let http_status = resp.status();
    if !http_status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Aristotle API error ({}): {}", http_status, body);
    }

    Ok(resp.json().await?)
}

/// Download the solution tar.gz and extract it to the output directory.
/// Returns the path where the solution was extracted.
pub async fn download_result(project_id: &str, output_dir: &Path) -> Result<PathBuf> {
    let key = api_key()?;
    let client = client();

    let resp = client
        .get(format!("{}/project/{}/result", BASE_URL, project_id))
        .header("X-API-Key", &key)
        .send()
        .await?;

    let http_status = resp.status();
    if !http_status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Aristotle API error ({}): {}", http_status, body);
    }

    let bytes = resp.bytes().await?;
    std::fs::create_dir_all(output_dir)?;

    // Extract tar.gz, stripping the top-level directory prefix
    // (Aristotle wraps results in a `project_aristotle/` directory)
    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();

        // Strip first path component (e.g. "project_aristotle/Test.lean" → "Test.lean")
        let stripped: PathBuf = path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        let dest = output_dir.join(&stripped);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Skip directories (create_dir_all handles them), extract files only
        if !entry.header().entry_type().is_dir() {
            entry.unpack(&dest)?;
        }
    }

    eprintln!("Solution extracted to {}", output_dir.display());
    Ok(output_dir.to_path_buf())
}

/// Cancel a running project.
pub async fn cancel(project_id: &str) -> Result<Project> {
    let key = api_key()?;
    let client = client();

    let resp = client
        .post(format!("{}/project/{}/cancel", BASE_URL, project_id))
        .header("X-API-Key", &key)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await?;

    let http_status = resp.status();
    if !http_status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Aristotle API error ({}): {}", http_status, body);
    }

    Ok(resp.json().await?)
}

/// List recent projects.
pub async fn list(limit: u32, status_filter: Option<&str>) -> Result<Vec<Project>> {
    let key = api_key()?;
    let client = client();

    let mut url = format!("{}/project?limit={}", BASE_URL, limit);
    if let Some(s) = status_filter {
        url.push_str(&format!("&status={}", s));
    }

    let resp = client
        .get(&url)
        .header("X-API-Key", &key)
        .send()
        .await?;

    let http_status = resp.status();
    if !http_status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Aristotle API error ({}): {}", http_status, body);
    }

    let list: ListResponse = resp.json().await?;
    Ok(list.projects)
}

/// High-level: submit a project, wait for completion, download the result.
pub async fn fill_sorry(
    project_dir: &Path,
    output_dir: &Path,
    prompt: &str,
    wait: bool,
    poll_interval_secs: Option<u64>,
) -> Result<()> {
    let project = submit(project_dir, prompt).await?;

    if !wait {
        eprintln!(
            "\nProject submitted. Check status with:\n  qedgen aristotle status {}",
            project.project_id
        );
        return Ok(());
    }

    eprintln!("\nWaiting for completion (this may take minutes to hours)...");
    let final_project = poll(&project.project_id, poll_interval_secs).await?;

    match final_project.status.as_str() {
        "COMPLETE" | "COMPLETE_WITH_ERRORS" => {
            if final_project.status == "COMPLETE_WITH_ERRORS" {
                eprintln!("Warning: Aristotle completed with some errors.");
            }
            download_result(&final_project.project_id, output_dir).await?;
            if let Some(summary) = &final_project.output_summary {
                eprintln!("\nSummary: {}", summary);
            }
        }
        status => {
            eprintln!("Project ended with status: {}", status);
            if let Some(summary) = &final_project.output_summary {
                eprintln!("Summary: {}", summary);
            }
        }
    }

    Ok(())
}
