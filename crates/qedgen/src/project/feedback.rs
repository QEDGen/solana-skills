//! `qedgen feedback` — structured GitHub-issue authoring: bundles version,
//! OS, runtime, last failure output, and a spec excerpt into a Markdown
//! body, then files via `gh` or prints a pre-filled URL. The local artifact
//! (`.qed/feedback/<ts>.md`) is written silently; the consent prompt fires
//! only at the remote-submission boundary. `capture_last_error` runs from
//! `main()`'s error path so the next invocation has real stderr to attach.

use anyhow::{anyhow, Context as _, Result};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream repo for feedback issues. Override via `QEDGEN_FEEDBACK_REPO`
/// (forks, internal mirrors).
const DEFAULT_FEEDBACK_REPO: &str = "QEDGen/solana-skills";

/// Cap on the complete GitHub web-URL fallback. Titles and bodies are
/// percent-encoded within this budget so user-controlled input cannot make
/// the fallback unusable.
const URL_FALLBACK_BUDGET: usize = 8000;
const URL_TITLE_BUDGET: usize = 1024;

/// What the user typed plus what we found on disk. Plain data — rendering
/// and submission are separate so `--dry-run` never touches the network.
pub struct FeedbackContext {
    pub qedgen_version: &'static str,
    pub os: String,
    pub arch: String,
    pub runtime: Option<String>,
    pub spec_path: Option<PathBuf>,
    pub spec_excerpt: Option<String>,
    pub last_error: Option<LastError>,
    pub user_note: Option<String>,
}

pub struct LastError {
    pub command: String,
    pub timestamp: String,
    pub stderr: String,
}

/// Entry point. Order: collect → render → preview → confirm → submit; each
/// step short-circuits so a user with no `gh` / internet still gets the
/// local artifact.
pub fn run(
    spec_path: Option<&Path>,
    note: Option<&str>,
    title: Option<&str>,
    dry_run: bool,
    yes: bool,
    no_open: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("read cwd")?;
    let ctx = collect(&cwd, spec_path, note)?;

    let resolved_title = title
        .map(str::to_string)
        .unwrap_or_else(|| default_title(&ctx));
    let resolved_title = sanitize_title(&resolved_title);
    let body = render_markdown(&ctx);

    if dry_run {
        println!("--- Title ---\n{resolved_title}\n");
        println!("--- Body ---\n{body}");
        return Ok(());
    }

    let saved = save_local_artifact(&cwd, &resolved_title, &body)?;
    eprintln!("Saved local copy to {}", saved.display());

    preview(&resolved_title, &body);

    if !yes && !confirm_remote_submit(&saved)? {
        eprintln!(
            "Skipping remote submission. The local artifact at {} can be \
             attached to an issue manually.",
            saved.display()
        );
        return Ok(());
    }

    let (approved_title, approved_body) =
        load_local_artifact(&saved).context("reload the reviewed feedback draft")?;
    let repo = resolve_repo();
    match submit_via_gh(&repo, &approved_title, &approved_body) {
        Ok(url) => {
            println!("Filed: {url}");
            Ok(())
        }
        Err(gh_err) => {
            eprintln!("gh CLI unavailable or failed: {gh_err}");
            let url = build_url_fallback(&repo, &approved_title, &approved_body)?;
            println!("Pre-filled issue URL:\n{url}");
            if !no_open {
                let _ = open_in_browser(&url);
            }
            Ok(())
        }
    }
}

/// Persist to `.qed/last-error.{log,json}` from main()'s error path.
/// `command` is the top-level subcommand name; the full stderr is captured
/// so panics and structured errors both land in the feedback bundle.
pub fn capture_last_error(workdir: &Path, command: &str, error: &anyhow::Error) -> Result<()> {
    let dir = workdir.join(".qed");
    fs::create_dir_all(&dir).ok();

    let now = chrono_like_timestamp();
    let stderr = redact_sensitive(&format!("{error:#}"));

    let log = dir.join("last-error.log");
    let body = format!(
        "command: qedgen {command}\ntimestamp: {now}\n\n{stderr}\n",
        command = command,
        now = now,
        stderr = stderr,
    );
    fs::write(&log, body)?;

    let json = dir.join("last-error.json");
    let payload = serde_json::json!({
        "command": command,
        "timestamp": now,
        "stderr": stderr,
    });
    fs::write(&json, serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn collect(cwd: &Path, spec_path: Option<&Path>, note: Option<&str>) -> Result<FeedbackContext> {
    let runtime = detect_runtime_label(cwd);
    let last_error = read_last_error(cwd).map(|mut error| {
        error.stderr = redact_sensitive(&error.stderr);
        error
    });
    let (resolved_spec, excerpt) = resolve_spec_excerpt(cwd, spec_path, last_error.as_ref());

    Ok(FeedbackContext {
        qedgen_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        runtime,
        spec_path: resolved_spec,
        spec_excerpt: excerpt,
        last_error,
        user_note: note.map(redact_sensitive),
    })
}

fn detect_runtime_label(cwd: &Path) -> Option<String> {
    // Best-effort: the label is only meaningful when not Unknown.
    let rt = crate::probe::detect_runtime_public(cwd);
    let label = format!("{rt:?}");
    if label == "Unknown" {
        None
    } else {
        Some(label)
    }
}

fn read_last_error(cwd: &Path) -> Option<LastError> {
    let log = cwd.join(".qed").join("last-error.log");
    let text = fs::read_to_string(&log).ok()?;
    let mut command = String::from("(unknown)");
    let mut timestamp = String::new();
    let mut stderr = String::new();
    let mut in_body = false;
    for line in text.lines() {
        if in_body {
            stderr.push_str(line);
            stderr.push('\n');
            continue;
        }
        if let Some(rest) = line.strip_prefix("command: qedgen ") {
            command = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("timestamp: ") {
            timestamp = rest.to_string();
        } else if line.is_empty() {
            in_body = true;
        }
    }
    Some(LastError {
        command,
        timestamp,
        stderr: stderr.trim_end().to_string(),
    })
}

fn resolve_spec_excerpt(
    cwd: &Path,
    explicit: Option<&Path>,
    last_error: Option<&LastError>,
) -> (Option<PathBuf>, Option<String>) {
    let path = explicit
        .map(|p| p.to_path_buf())
        .or_else(|| find_spec_in_error(last_error))
        .or_else(|| find_default_spec(cwd));
    let Some(p) = path else {
        return (None, None);
    };
    let abs = if p.is_absolute() {
        p.clone()
    } else {
        cwd.join(&p)
    };
    let text = match fs::read_to_string(&abs) {
        Ok(t) => t,
        Err(_) => return (Some(p), None),
    };
    // Redact the complete document first so a line-window cannot drop a PEM
    // header and expose the remaining key material.
    let text = redact_sensitive(&text);
    let excerpt = excerpt_relevant(&text, last_error);
    (Some(p), Some(excerpt))
}

fn find_default_spec(cwd: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(cwd).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("qedspec") {
            return Some(path);
        }
    }
    None
}

fn find_spec_in_error(last_error: Option<&LastError>) -> Option<PathBuf> {
    let err = last_error?;
    // Cheap heuristic: pull the first `.qedspec` token from stderr — wrong
    // matches are harmless (the excerpt step must also read the file).
    for token in err.stderr.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '`' | ':'));
        if trimmed.ends_with(".qedspec") {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

fn excerpt_relevant(spec: &str, last_error: Option<&LastError>) -> String {
    // If the stderr mentions a line number, surface ±10 lines around it;
    // otherwise return the first 60 lines so the body never balloons.
    if let Some(err) = last_error {
        if let Some(line) = parse_line_hint(&err.stderr) {
            return surrounding_lines(spec, line, 10);
        }
    }
    let head: Vec<&str> = spec.lines().take(60).collect();
    let trailer = if spec.lines().count() > 60 {
        "\n…(truncated; full spec available locally)"
    } else {
        ""
    };
    format!("{}{}", head.join("\n"), trailer)
}

fn parse_line_hint(stderr: &str) -> Option<usize> {
    // Match `:NN:` (filename:line:col) and `line NN`. First hit wins.
    for token in stderr.split(|c: char| c == ':' || c.is_whitespace()) {
        if let Ok(n) = token.parse::<usize>() {
            if n > 0 && n < 100_000 {
                return Some(n);
            }
        }
    }
    None
}

fn surrounding_lines(text: &str, line: usize, window: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = line.saturating_sub(window).saturating_sub(1);
    let end = (line + window).min(lines.len());
    let mut out = String::new();
    for (i, content) in lines[start..end].iter().enumerate() {
        let n = start + i + 1;
        let marker = if n == line { ">" } else { " " };
        out.push_str(&format!("{marker} {n:>4} | {content}\n"));
    }
    out.trim_end().to_string()
}

fn render_markdown(ctx: &FeedbackContext) -> String {
    let mut out = String::new();

    if let Some(note) = &ctx.user_note {
        out.push_str("## What happened\n\n");
        out.push_str(note.trim());
        out.push_str("\n\n");
    } else {
        out.push_str("## What happened\n\n");
        out.push_str("_(describe the unexpected behavior here — what you ran, what you expected, what you got)_\n\n");
    }

    out.push_str("## Environment\n\n");
    out.push_str(&format!("- qedgen: `{}`\n", ctx.qedgen_version));
    out.push_str(&format!("- os/arch: `{}/{}`\n", ctx.os, ctx.arch));
    out.push_str(&format!(
        "- runtime: `{}`\n",
        ctx.runtime.as_deref().unwrap_or("not-detected")
    ));
    out.push('\n');

    if let Some(err) = &ctx.last_error {
        out.push_str("## Last error\n\n");
        out.push_str(&format!(
            "Command: `qedgen {}` ({})\n\n",
            err.command, err.timestamp
        ));
        out.push_str("```\n");
        out.push_str(truncate(&err.stderr, 4000));
        out.push_str("\n```\n\n");
    }

    if let Some(excerpt) = &ctx.spec_excerpt {
        let path_label = ctx
            .spec_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".qedspec".to_string());
        let path_label = sanitize_title(&path_label);
        out.push_str(&format!("## Spec excerpt (`{}`)\n\n", path_label));
        out.push_str("```\n");
        out.push_str(truncate(excerpt, 3000));
        out.push_str("\n```\n\n");
    }

    out.push_str("---\n");
    out.push_str("_Filed via `qedgen feedback`. Known secret patterns are redacted heuristically; review the local draft before submitting. The spec excerpt above is the section nearest to the failure; the full spec is withheld by default._\n");
    out
}

fn default_title(ctx: &FeedbackContext) -> String {
    if let Some(err) = &ctx.last_error {
        let first_line = err.stderr.lines().next().unwrap_or("").trim();
        let snippet = truncate(first_line, 80);
        format!(
            "[qedgen {}] {} failed: {}",
            ctx.qedgen_version, err.command, snippet
        )
    } else {
        format!("[qedgen {}] feedback", ctx.qedgen_version)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Step back to a char boundary so we never split a UTF-8 sequence.
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn sanitize_title(title: &str) -> String {
    redact_sensitive(title)
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

fn redact_sensitive(input: &str) -> String {
    let patterns = [
        r"(?s)-----BEGIN [^-\n]+-----.*?(?:-----END [^-\n]+-----|$)",
        r"\b(?:gh[pousr]_[A-Za-z0-9_]{10,}|github_pat_[A-Za-z0-9_]{10,})\b",
        r"\b(?:sk|xox[baprs])-[A-Za-z0-9_-]{10,}\b",
        r"\bAKIA[0-9A-Z]{16}\b",
        r#"(?i)(?:["']?\b[A-Za-z0-9_-]*(?:password|passwd|secret|api[_-]?key|access[_-]?token|auth[_-]?token|private[_-]?key)[A-Za-z0-9_-]*\b["']?)\s*[:=]\s*(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\[[^\]]*\]|[^,;\r\n]+)"#,
        r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{12,}",
    ];
    patterns.iter().fold(input.to_string(), |value, pattern| {
        let re = regex::Regex::new(pattern).expect("feedback redaction pattern is valid");
        re.replace_all(&value, |caps: &regex::Captures<'_>| {
            let newlines = caps[0].bytes().filter(|b| *b == b'\n').count();
            format!("[REDACTED]{}", "\n".repeat(newlines))
        })
        .into_owned()
    })
}

fn save_local_artifact(cwd: &Path, title: &str, body: &str) -> Result<PathBuf> {
    let dir = cwd.join(".qed").join("feedback");
    fs::create_dir_all(&dir).context("create .qed/feedback")?;
    let stamp = chrono_like_timestamp().replace(':', "-");
    let path = dir.join(format!("{stamp}.md"));
    let mut f = fs::File::create(&path)?;
    writeln!(f, "# {}", sanitize_title(title))?;
    writeln!(f)?;
    f.write_all(redact_sensitive(body).as_bytes())?;
    Ok(path)
}

fn load_local_artifact(path: &Path) -> Result<(String, String)> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .replace("\r\n", "\n");
    let (title_line, body) = text
        .split_once("\n\n")
        .ok_or_else(|| anyhow!("feedback draft is missing its title/body separator"))?;
    let title = title_line
        .strip_prefix("# ")
        .ok_or_else(|| anyhow!("feedback draft must begin with '# <title>'"))?;
    let title = sanitize_title(title);
    let body = redact_sensitive(body);
    let normalized = format!("# {title}\n\n{body}");
    if normalized != text {
        fs::write(path, normalized).with_context(|| format!("sanitize {}", path.display()))?;
    }
    Ok((title, body))
}

fn preview(title: &str, body: &str) {
    eprintln!();
    eprintln!("------ Issue preview ------");
    eprintln!("Title: {title}");
    eprintln!();
    eprintln!("{}", body);
    eprintln!("---------------------------");
}

fn confirm_remote_submit(draft: &Path) -> Result<bool> {
    use std::io::{stdin, BufRead, IsTerminal};
    eprint!(
        "Review or edit {} then file this as a public GitHub issue? [y/N] ",
        draft.display()
    );
    std::io::stderr().flush().ok();

    // Non-interactive shells default to "no" — pipelines and CI should
    // never silently post issues. Users in those environments pass --yes
    // explicitly.
    if !stdin().is_terminal() {
        eprintln!("(non-interactive; defaulting to no)");
        return Ok(false);
    }

    let mut line = String::new();
    stdin().lock().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

fn resolve_repo() -> String {
    std::env::var("QEDGEN_FEEDBACK_REPO").unwrap_or_else(|_| DEFAULT_FEEDBACK_REPO.to_string())
}

fn submit_via_gh(repo: &str, title: &str, body: &str) -> Result<String> {
    if Command::new("gh").arg("--version").output().is_err() {
        return Err(anyhow!("gh CLI not installed"));
    }
    let output = Command::new("gh")
        .args([
            "issue", "create", "--repo", repo, "--title", title, "--body", body,
        ])
        .output()
        .context("invoke gh issue create")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("gh exit {}: {}", output.status, err.trim()));
    }
    let url = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|l| l.starts_with("https://"))
        .map(str::to_string)
        .unwrap_or_else(|| String::from_utf8_lossy(&output.stdout).trim().to_string());
    Ok(url)
}

fn build_url_fallback(repo: &str, title: &str, body: &str) -> Result<String> {
    let prefix = format!("https://github.com/{repo}/issues/new?title=");
    let body_key = "&body=";
    let query_budget = URL_FALLBACK_BUDGET
        .checked_sub(prefix.len() + body_key.len())
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| anyhow!("feedback repository is too long for the URL fallback"))?;

    let title_budget = query_budget.min(URL_TITLE_BUDGET);
    let title_encoded = percent_encode_with_budget(title, title_budget, "…");
    let body_budget = query_budget - title_encoded.len();
    let body_encoded = percent_encode_with_budget(
        body,
        body_budget,
        "\n\n_(body truncated for URL — see local .qed/feedback/ for full version)_",
    );
    let url = format!("{prefix}{title_encoded}{body_key}{body_encoded}");
    debug_assert!(url.len() <= URL_FALLBACK_BUDGET);
    Ok(url)
}

fn percent_encode_with_budget(value: &str, budget: usize, suffix: &str) -> String {
    let encoded = percent_encode(value);
    if encoded.len() <= budget {
        return encoded;
    }

    let encoded_suffix = percent_encode(suffix);
    if encoded_suffix.len() > budget {
        return String::new();
    }

    let mut boundaries: Vec<usize> = value.char_indices().map(|(i, _)| i).collect();
    boundaries.push(value.len());
    let mut lo = 0usize;
    let mut hi = boundaries.len() - 1;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let end = boundaries[mid];
        if percent_encode(&value[..end]).len() + encoded_suffix.len() <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!(
        "{}{encoded_suffix}",
        percent_encode(&value[..boundaries[lo]])
    )
}

fn open_in_browser(url: &str) -> Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "start"
    } else {
        "xdg-open"
    };
    Command::new(cmd).arg(url).status().ok();
    Ok(())
}

/// Minimal RFC 3986 percent-encoder — avoids a dep; input is our own
/// title/body text, so the full URI grammar isn't needed.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// ISO-8601-ish timestamp via the existing `time` dep; falls back to UNIX
/// seconds.
fn chrono_like_timestamp() -> String {
    use time::format_description::well_known::Iso8601;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&Iso8601::DEFAULT)
        .unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn percent_encode_keeps_unreserved() {
        assert_eq!(percent_encode("abc-123_~"), "abc-123_~");
    }

    #[test]
    fn percent_encode_escapes_space_and_special() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("&="), "%26%3D");
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        let s = "abcé";
        assert_eq!(truncate(s, 4), "abc");
        assert_eq!(truncate(s, 10), "abcé");
    }

    #[test]
    fn render_markdown_includes_version_and_error() {
        let ctx = FeedbackContext {
            qedgen_version: "2.23.0",
            os: "macos".into(),
            arch: "aarch64".into(),
            runtime: Some("Anchor".into()),
            spec_path: None,
            spec_excerpt: None,
            last_error: Some(LastError {
                command: "check".into(),
                timestamp: "2026-05-21T00:00:00Z".into(),
                stderr: "lint: missing MathOverflow variant".into(),
            }),
            user_note: Some("`qedgen check` reports a missing variant I don't expect".into()),
        };
        let body = render_markdown(&ctx);
        assert!(body.contains("qedgen: `2.23.0`"));
        assert!(body.contains("Command: `qedgen check`"));
        assert!(body.contains("missing MathOverflow variant"));
        assert!(body.contains("`qedgen check` reports"));
    }

    #[test]
    fn render_markdown_omits_absolute_workstation_path() {
        let ctx = FeedbackContext {
            qedgen_version: "2.23.0",
            os: "macos".into(),
            arch: "aarch64".into(),
            runtime: None,
            spec_path: None,
            spec_excerpt: None,
            last_error: None,
            user_note: Some("something failed".into()),
        };
        let body = render_markdown(&ctx);
        assert!(!body.contains("/Users/alice/private-project"));
    }

    #[test]
    fn capture_last_error_writes_log_and_json() {
        let tmp = tempdir().unwrap();
        let err = anyhow!("boom; password=top-secret");
        capture_last_error(tmp.path(), "check", &err).unwrap();
        let log = fs::read_to_string(tmp.path().join(".qed").join("last-error.log")).unwrap();
        assert!(log.contains("command: qedgen check"));
        assert!(log.contains("boom"));
        assert!(!log.contains("top-secret"));
        let json = fs::read_to_string(tmp.path().join(".qed").join("last-error.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["command"], "check");
        assert!(!json.contains("top-secret"));
    }

    #[test]
    fn read_last_error_round_trip() {
        let tmp = tempdir().unwrap();
        let err = anyhow!("parse: unexpected token at line 12\n  context here");
        capture_last_error(tmp.path(), "check", &err).unwrap();
        let last = read_last_error(tmp.path()).unwrap();
        assert_eq!(last.command, "check");
        assert!(last.stderr.contains("unexpected token"));
        assert!(last.stderr.contains("context here"));
    }

    #[test]
    fn parse_line_hint_picks_first_number() {
        assert_eq!(parse_line_hint("error at line 42 col 7"), Some(42));
        assert_eq!(parse_line_hint("file.qedspec:99:3: bad"), Some(99));
        assert_eq!(parse_line_hint("no line hint"), None);
    }

    #[test]
    fn surrounding_lines_marks_target() {
        let text = "a\nb\nc\nd\ne\nf\ng";
        let out = surrounding_lines(text, 3, 1);
        assert!(out.contains(">    3 | c"));
        assert!(out.contains("    2 | b"));
        assert!(out.contains("    4 | d"));
    }

    #[test]
    fn url_fallback_encodes_and_caps() {
        let huge = "x".repeat(20_000);
        let url = build_url_fallback("o/r", "T E", &huge).unwrap();
        assert!(url.starts_with("https://github.com/o/r/issues/new?title=T%20E&body="));
        assert!(url.contains("body%20truncated"));
        assert!(url.len() <= URL_FALLBACK_BUDGET);
    }

    #[test]
    fn url_fallback_includes_title_and_repo_in_total_budget() {
        let title = "title with spaces ".repeat(1_000);
        let body = "body with spaces ".repeat(1_000);
        let url = build_url_fallback("owner/repository", &title, &body).unwrap();
        assert!(
            url.len() <= URL_FALLBACK_BUDGET,
            "url length was {}",
            url.len()
        );
    }

    #[test]
    fn url_fallback_rejects_repo_that_exhausts_total_budget() {
        let fixed_url_len = "https://github.com//issues/new?title=".len() + "&body=".len();
        let repo = "r".repeat(URL_FALLBACK_BUDGET - fixed_url_len);
        let err = build_url_fallback(&repo, "title", "body").unwrap_err();
        assert!(err.to_string().contains("repository is too long"));
    }

    #[test]
    fn default_title_uses_last_error_command() {
        let ctx = FeedbackContext {
            qedgen_version: "2.23.0",
            os: "linux".into(),
            arch: "x86_64".into(),
            runtime: None,
            spec_path: None,
            spec_excerpt: None,
            last_error: Some(LastError {
                command: "codegen".into(),
                timestamp: "t".into(),
                stderr: "panicked at: missing field".into(),
            }),
            user_note: None,
        };
        let title = default_title(&ctx);
        assert!(title.contains("codegen failed"));
        assert!(title.contains("2.23.0"));
    }

    #[test]
    fn redact_sensitive_masks_tokens_keys_and_secret_assignments() {
        let raw = "ghp_1234567890abcdef sk-live-1234567890 AKIA1234567890ABCDEF \
                   password=top-secret api_key: abc123 AWS_SECRET_ACCESS_KEY=aws-secret \
                   Bearer abcdefghijklmnop -----BEGIN PRIVATE KEY-----";
        let redacted = redact_sensitive(raw);
        assert!(!redacted.contains("ghp_1234567890abcdef"));
        assert!(!redacted.contains("sk-live-1234567890"));
        assert!(!redacted.contains("AKIA1234567890ABCDEF"));
        assert!(!redacted.contains("top-secret"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("aws-secret"));
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(!redacted.contains("BEGIN PRIVATE KEY"));
        assert!(redacted.matches("[REDACTED]").count() >= 4);
    }

    #[test]
    fn redact_sensitive_masks_quoted_json_and_private_key_values() {
        let raw = r#"{"password":"violet-cactus-987","api_key":"orchid-winter-876","private_key":["one","two"]}"#;
        let redacted = redact_sensitive(raw);
        assert!(!redacted.contains("violet-cactus-987"));
        assert!(!redacted.contains("orchid-winter-876"));
        assert!(!redacted.contains("one"));
        assert!(!redacted.contains("two"));
    }

    #[test]
    fn redact_sensitive_masks_quoted_values_with_spaces() {
        let redacted = redact_sensitive(r#"password="violet cactus 987""#);
        assert!(!redacted.contains("violet cactus 987"));
    }

    #[test]
    fn redact_sensitive_masks_unquoted_values_through_record_boundary() {
        let redacted = redact_sensitive(
            "password = correct horse battery staple\nstatus = authentication failed",
        );
        assert!(!redacted.contains("horse battery staple"));
        assert!(redacted.contains("status = authentication failed"));
    }

    #[test]
    fn spec_excerpt_redacts_pem_before_selecting_line_window() {
        let spec = "header\nline2\n-----BEGIN PRIVATE KEY-----\nsecret-material\n-----END PRIVATE KEY-----\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\nline16\nline17\nline18\nline19\nline20\n";
        let sanitized = redact_sensitive(spec);
        let excerpt = surrounding_lines(&sanitized, 4, 1);
        assert!(!excerpt.contains("secret-material"));
        assert!(!excerpt.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn load_local_artifact_returns_the_edited_title_and_body() {
        let tmp = tempdir().unwrap();
        let path = save_local_artifact(tmp.path(), "Original title", "Original body").unwrap();
        fs::write(&path, "# Edited title\n\nEdited body\n").unwrap();
        let (title, body) = load_local_artifact(&path).unwrap();
        assert_eq!(title, "Edited title");
        assert_eq!(body, "Edited body\n");
    }

    #[test]
    fn saved_artifact_is_redacted_before_persistence() {
        let tmp = tempdir().unwrap();
        let path = save_local_artifact(
            tmp.path(),
            "password=title-secret",
            "stderr: api_key=body-secret",
        )
        .unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("title-secret"));
        assert!(!text.contains("body-secret"));
    }

    #[test]
    fn edited_draft_drives_url_fallback_payload() {
        let tmp = tempdir().unwrap();
        let path = save_local_artifact(tmp.path(), "Original", "Original body").unwrap();
        fs::write(&path, "# Edited\n\nEdited body").unwrap();
        let (title, body) = load_local_artifact(&path).unwrap();
        let url = build_url_fallback("owner/repo", &title, &body).unwrap();
        assert!(url.contains("Edited"));
        assert!(url.contains("Edited%20body"));
        assert!(!url.contains("Original%20body"));
    }

    #[test]
    fn load_local_artifact_accepts_crlf_markdown() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        fs::write(&path, "# Edited\r\n\r\nEdited body\r\n").unwrap();
        let (title, body) = load_local_artifact(&path).unwrap();
        assert_eq!(title, "Edited");
        assert_eq!(body, "Edited body\n");
    }

    #[test]
    fn url_fallback_budgets_encoded_unicode_body() {
        let body = "😀,!?".repeat(2_000);
        let url = build_url_fallback("o/r", "title", &body).unwrap();
        assert!(url.len() <= 8_000, "url length was {}", url.len());
        assert!(url.contains("body%20truncated"));
    }
}
