//! `qedgen feedback` — structured GitHub-issue authoring from the CLI.
//!
//! Bundles a small, predictable context envelope (qedgen version, OS,
//! detected runtime, last command's failure output, and an optional spec
//! excerpt) into a Markdown body and either files the issue via the `gh`
//! CLI or prints a pre-filled GitHub web URL. Per
//! `[[feedback_tactile_tooling]]`, the local artifact (`.qed/feedback/
//! <timestamp>.md`) is written silently — the consent prompt fires only
//! at the remote-submission boundary.
//!
//! Companion: `capture_last_error` is called from `main()`'s error path
//! so the next `qedgen feedback` invocation has real stderr to attach.
//! Without this hook the feedback body would be context-free.

use anyhow::{anyhow, Context as _, Result};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream repo for feedback issues. Override via `QEDGEN_FEEDBACK_REPO`
/// (forks, internal mirrors). Default points at the public skill repo so
/// users land on a triage-ready issue tracker.
const DEFAULT_FEEDBACK_REPO: &str = "QEDGen/solana-skills";

/// Hard cap on the GitHub web URL fallback. The actual GitHub limit is
/// ~8 KB but we keep margin for the title + query keys + percent
/// expansion. Body is truncated with a marker if it would push past
/// this; the user can paste the full version into the issue body once
/// the browser opens.
const URL_BODY_BUDGET: usize = 6500;

/// What the user typed (`Title: …` becomes the issue title) plus what
/// we found on disk. Stays plain data — rendering and submission are
/// separate so the `--dry-run` path can print the body without touching
/// the network.
pub struct FeedbackContext {
    pub qedgen_version: &'static str,
    pub os: String,
    pub arch: String,
    pub cwd: PathBuf,
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

/// Entry point for the `qedgen feedback` subcommand.
///
/// Order: collect → render → preview → confirm → submit. Each step
/// short-circuits cleanly on failure so a user with no `gh` and no
/// internet still gets the local `.qed/feedback/<ts>.md` artifact they
/// can attach manually.
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
    let body = render_markdown(&ctx);

    if dry_run {
        println!("--- Title ---\n{resolved_title}\n");
        println!("--- Body ---\n{body}");
        return Ok(());
    }

    let saved = save_local_artifact(&cwd, &resolved_title, &body)?;
    eprintln!("Saved local copy to {}", saved.display());

    preview(&resolved_title, &body);

    if !yes && !confirm_remote_submit()? {
        eprintln!(
            "Skipping remote submission. The local artifact at {} can be \
             attached to an issue manually.",
            saved.display()
        );
        return Ok(());
    }

    let repo = resolve_repo();
    match submit_via_gh(&repo, &resolved_title, &body) {
        Ok(url) => {
            println!("Filed: {url}");
            Ok(())
        }
        Err(gh_err) => {
            eprintln!("gh CLI unavailable or failed: {gh_err}");
            let url = build_url_fallback(&repo, &resolved_title, &body);
            println!("Pre-filled issue URL:\n{url}");
            if !no_open {
                let _ = open_in_browser(&url);
            }
            Ok(())
        }
    }
}

/// Persisted to `.qed/last-error.{log,json}` from main()'s error path.
/// `command` is the top-level subcommand name (`check`, `codegen`, …);
/// the full stderr is captured so panics and structured errors both
/// land in the feedback bundle without the user having to scroll their
/// terminal.
pub fn capture_last_error(workdir: &Path, command: &str, error: &anyhow::Error) -> Result<()> {
    let dir = workdir.join(".qed");
    fs::create_dir_all(&dir).ok();

    let now = chrono_like_timestamp();
    let stderr = format!("{error:#}");

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
    let last_error = read_last_error(cwd);
    let (resolved_spec, excerpt) = resolve_spec_excerpt(cwd, spec_path, last_error.as_ref());

    Ok(FeedbackContext {
        qedgen_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cwd: cwd.to_path_buf(),
        runtime,
        spec_path: resolved_spec,
        spec_excerpt: excerpt,
        last_error,
        user_note: note.map(str::to_string),
    })
}

fn detect_runtime_label(cwd: &Path) -> Option<String> {
    // Best-effort: probe::detect_runtime_public returns a value for every
    // directory, but the label is only meaningful when it's not Unknown.
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
    // Cheap heuristic: the lint and parser error messages quote the spec
    // path. We pull the first `.qedspec` token out of the stderr — wrong
    // matches are harmless because the excerpt step also requires the
    // file to read.
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
    out.push_str(&format!("- cwd: `{}`\n\n", ctx.cwd.display()));

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
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".qedspec".to_string());
        out.push_str(&format!("## Spec excerpt (`{}`)\n\n", path_label));
        out.push_str("```\n");
        out.push_str(truncate(excerpt, 3000));
        out.push_str("\n```\n\n");
    }

    out.push_str("---\n");
    out.push_str("_Filed via `qedgen feedback`. The spec excerpt above is the section nearest to the failure; full spec withheld by default. Add `--include-spec`-equivalent context here if helpful._\n");
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

fn save_local_artifact(cwd: &Path, title: &str, body: &str) -> Result<PathBuf> {
    let dir = cwd.join(".qed").join("feedback");
    fs::create_dir_all(&dir).context("create .qed/feedback")?;
    let stamp = chrono_like_timestamp().replace(':', "-");
    let path = dir.join(format!("{stamp}.md"));
    let mut f = fs::File::create(&path)?;
    writeln!(f, "# {title}")?;
    writeln!(f)?;
    f.write_all(body.as_bytes())?;
    Ok(path)
}

fn preview(title: &str, body: &str) {
    eprintln!();
    eprintln!("------ Issue preview ------");
    eprintln!("Title: {title}");
    eprintln!();
    eprintln!("{}", truncate(body, 2000));
    if body.len() > 2000 {
        eprintln!("...(truncated for preview; full body in local artifact)");
    }
    eprintln!("---------------------------");
}

fn confirm_remote_submit() -> Result<bool> {
    use std::io::{stdin, BufRead, IsTerminal};
    eprint!("File this as a public GitHub issue? [y/N] ");
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

fn build_url_fallback(repo: &str, title: &str, body: &str) -> String {
    let truncated = truncate(body, URL_BODY_BUDGET);
    let suffix = if body.len() > URL_BODY_BUDGET {
        "\n\n_(body truncated for URL — see local .qed/feedback/ for full version)_"
    } else {
        ""
    };
    format!(
        "https://github.com/{repo}/issues/new?title={}&body={}",
        percent_encode(title),
        percent_encode(&format!("{truncated}{suffix}")),
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

/// Minimal RFC 3986 percent-encoder. Avoids a new dep this late in the
/// release cycle; the input is always our own title/body text so the
/// reserved-character set doesn't need to cover the full URI grammar.
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

/// ISO-8601-ish timestamp without pulling chrono. `time` is already in
/// the workspace deps so we use it for formatting, but the call surface
/// is tiny and gracefully falls back to UNIX seconds.
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
            cwd: PathBuf::from("/tmp/proj"),
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
    fn capture_last_error_writes_log_and_json() {
        let tmp = tempdir().unwrap();
        let err = anyhow!("boom");
        capture_last_error(tmp.path(), "check", &err).unwrap();
        let log = fs::read_to_string(tmp.path().join(".qed").join("last-error.log")).unwrap();
        assert!(log.contains("command: qedgen check"));
        assert!(log.contains("boom"));
        let json = fs::read_to_string(tmp.path().join(".qed").join("last-error.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["command"], "check");
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
        let url = build_url_fallback("o/r", "T E", &huge);
        assert!(url.starts_with("https://github.com/o/r/issues/new?title=T%20E&body="));
        assert!(url.contains("body%20truncated"));
        assert!(url.len() < 12_000); // Encoded length stays bounded.
    }

    #[test]
    fn default_title_uses_last_error_command() {
        let ctx = FeedbackContext {
            qedgen_version: "2.23.0",
            os: "linux".into(),
            arch: "x86_64".into(),
            cwd: PathBuf::from("."),
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
}
