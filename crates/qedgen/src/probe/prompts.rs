//! Interview prompts file writer (v2.19 M1.5).
//!
//! Renders a `Vec<Cluster>` to a markdown file the user edits to ratify
//! the scaffold-to-spec interview. Per the
//! `feedback_tactile_tooling`/`feedback_no_anchor_v2_mentions` doctrines:
//! file-based, reversible via git, harness-agnostic. The user opens
//! `.qed/audit/<ts>/interview.md`, checks one option per cluster (`[x]`),
//! optionally adds notes, then re-invokes the auditor to lift accepted
//! clauses into the spec.
//!
//! Layout — one section per cluster, plus a header and footer.
//! Each cluster's `question_md` field is already pre-rendered by
//! `cluster::render_template`; this module just stitches them together
//! with consistent surrounding chrome so the file is reviewable as a
//! whole.
//!
//! M1.6 adds the inverse direction: `read_interview` parses the
//! user-edited file and returns the ratified choices for each cluster.

use crate::cluster::{Cluster, Confidence};
use anyhow::{Context, Result};
use std::path::Path;

/// Render the full interview markdown.
///
/// `program_name` appears in the title; pass the spec's canonical name
/// (or the directory name in spec-less mode). `now_iso` is an ISO-8601
/// UTC timestamp written into the header — passed in (not read from
/// `chrono::now`) so tests are deterministic.
pub fn render_interview(clusters: &[Cluster], program_name: &str, now_iso: &str) -> String {
    let mut s = String::new();
    write_header(&mut s, program_name, now_iso, clusters.len());

    if clusters.is_empty() {
        s.push_str("\n_No candidate spec clauses were extracted from this program._\n\n");
        s.push_str(
            "This is the auditor's silent outcome when no `_unchecked` sites, \
             no missing-check patterns, and no probe convergence apply. Re-run \
             the probe with `--bootstrap` if you need a category-aware audit \
             checklist instead.\n",
        );
        write_footer(&mut s);
        return s;
    }

    // High-confidence clusters first (most signal); within a confidence
    // band, the clusters arrive already sorted by `cluster_protos`
    // (Program-scope before Handler-scope, then by kind).
    let mut by_conf: [Vec<&Cluster>; 3] = Default::default();
    for c in clusters {
        let idx = match c.confidence {
            Confidence::High => 0,
            Confidence::Medium => 1,
            Confidence::Low => 2,
        };
        by_conf[idx].push(c);
    }

    let band_labels = [
        (
            "## High-confidence clusters",
            "high-confidence",
            Confidence::High,
        ),
        (
            "## Medium-confidence clusters",
            "medium-confidence",
            Confidence::Medium,
        ),
        (
            "## Low-confidence clusters",
            "low-confidence",
            Confidence::Low,
        ),
    ];
    for (heading, _slug, conf) in &band_labels {
        let band = match conf {
            Confidence::High => &by_conf[0],
            Confidence::Medium => &by_conf[1],
            Confidence::Low => &by_conf[2],
        };
        if band.is_empty() {
            continue;
        }
        s.push_str(heading);
        s.push_str("\n\n");
        for cluster in band {
            write_cluster_block(&mut s, cluster);
        }
    }

    write_footer(&mut s);
    s
}

fn write_header(s: &mut String, program: &str, now_iso: &str, n_clusters: usize) {
    s.push_str(&format!("# Spec interview — {}\n\n", program));
    s.push_str(&format!("_Generated: {}_\n\n", now_iso));
    s.push_str(&format!(
        "The probe identified **{} candidate spec clause{}**. For each \
         one, check ONE option below by replacing `[ ]` with `[x]`. Add \
         rationale under `_notes:_` where useful — the rejection path \
         and bug path both benefit from a one-line reason.\n\n",
        n_clusters,
        if n_clusters == 1 { "" } else { "s" }
    ));
    s.push_str("Options across every cluster:\n\n");
    s.push_str("- **accept** — emit the suggested clause into the generated `.qedspec`\n");
    s.push_str(
        "- **narrow** (program-scope clusters only) — emit per-handler `requires` \
         clauses instead of a single program invariant\n",
    );
    s.push_str(
        "- **reject** — the proposed clause is an over-claim; drop it and note \
         the reason\n",
    );
    s.push_str(
        "- **bug** — the implicit precondition is real but NOT enforced anywhere; \
         flag as a finding rather than a spec clause\n\n",
    );
    s.push_str("---\n\n");
}

fn write_cluster_block(s: &mut String, c: &Cluster) {
    // The cluster's question_md already has the `## <label>` header and
    // option checkboxes. Wrap it with a metadata block (cluster id,
    // evidence count, suggested syntax) for transparency, and inject
    // the confidence/scope-banner before the prompt body.
    s.push_str(&format!("<!-- cluster: {} -->\n", c.id));
    s.push_str(&format!(
        "_confidence: **{:?}** · scope: **{}** · evidence: **{}** finding{}_\n\n",
        c.confidence,
        c.scope.as_key(),
        c.evidence_count,
        if c.evidence_count == 1 { "" } else { "s" },
    ));
    s.push_str(&c.question_md);
    s.push_str("\n<details><summary>Suggested spec syntax (renders on accept)</summary>\n\n");
    s.push_str("```\n");
    s.push_str(&c.suggested_syntax);
    if !c.suggested_syntax.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("```\n\n");
    s.push_str("</details>\n\n");
    s.push_str("---\n\n");
}

fn write_footer(s: &mut String) {
    s.push_str("## When done\n\n");
    s.push_str(
        "Save this file, then re-invoke the auditor (`/audit` or \
         `qedgen audit --resume`). The auditor reads your choices, writes \
         accepted clauses to `<program>.qedspec`, rejected ones to \
         `.qed/plan/scoping.md`, and flags `bug` choices into \
         `.qed/findings/`.\n\n",
    );
    s.push_str(
        "Unchecked questions are treated as **deferred** — leaving the \
         file partially-answered is fine; the auditor only acts on \
         clusters with exactly one option checked.\n",
    );
}

// ============================================================================
// Reader (M1.6) — parse the user-edited interview.md
// ============================================================================

/// One ratified-choice record per cluster. `Some(Choice)` if the user
/// checked exactly one option; `None` if zero or more than one were
/// checked (the latter logged as a warning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ratification {
    pub cluster_id: String,
    pub choice: Option<Choice>,
    /// User-written rationale captured from the `_notes:_` block. Empty
    /// when the user left the notes empty (the auditor doesn't require
    /// notes — they're optional context).
    pub notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Accept,
    Narrow,
    Reject,
    Bug,
}

/// Parse an interview markdown file (or string content).
pub fn read_interview_file(path: &Path) -> Result<Vec<Ratification>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read interview file at {}", path.display()))?;
    Ok(read_interview(&content))
}

/// Parse interview markdown content into per-cluster ratifications.
///
/// Tolerant parser: accepts `[x]`/`[X]`, `**bold**` variations, and
/// arbitrary content between cluster sections. Unchecked clusters
/// produce `Ratification { choice: None, … }` — the auditor's
/// downstream pass skips these as "deferred."
pub fn read_interview(content: &str) -> Vec<Ratification> {
    let mut out = Vec::new();

    // Each cluster block is anchored by an HTML comment of the form
    // `<!-- cluster: <id> -->`. We split the document at these anchors
    // and parse each segment as one cluster's content.
    let mut current: Option<RatificationBuilder> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(id) = parse_cluster_anchor(trimmed) {
            // Close out the previous cluster, if any, then start a
            // fresh one.
            if let Some(prev) = current.take() {
                out.push(prev.finish());
            }
            current = Some(RatificationBuilder::new(id));
            continue;
        }
        if let Some(b) = current.as_mut() {
            b.consume_line(line);
        }
    }
    if let Some(last) = current.take() {
        out.push(last.finish());
    }
    out
}

fn parse_cluster_anchor(line: &str) -> Option<String> {
    let s = line.strip_prefix("<!-- cluster: ")?.strip_suffix(" -->")?;
    Some(s.to_string())
}

struct RatificationBuilder {
    cluster_id: String,
    checked: Vec<Choice>,
    notes_started: bool,
    notes_terminated: bool,
    notes: String,
}

impl RatificationBuilder {
    fn new(cluster_id: String) -> Self {
        Self {
            cluster_id,
            checked: Vec::new(),
            notes_started: false,
            notes_terminated: false,
            notes: String::new(),
        }
    }

    fn consume_line(&mut self, line: &str) {
        // Order matters: notes detection runs before checkbox detection
        // so that a `[x]` inside a notes block isn't mis-read.
        if self.notes_started && !self.notes_terminated {
            // Terminate the notes block at the next `<details>`, `---`,
            // or another markdown section start.
            let trimmed = line.trim_start();
            if trimmed.starts_with("<details")
                || trimmed.starts_with("---")
                || trimmed.starts_with("## ")
            {
                self.notes_terminated = true;
            } else {
                self.notes.push_str(line);
                self.notes.push('\n');
                return;
            }
        }

        if !self.notes_started && line.trim().starts_with("_notes:_") {
            self.notes_started = true;
            return;
        }

        if let Some(choice) = parse_checked_option(line) {
            self.checked.push(choice);
        }
    }

    fn finish(self) -> Ratification {
        let trimmed_notes = self.notes.trim();
        // If user typed >1 checked option, return None — the orchestrator
        // logs a warning and treats the cluster as deferred. v2.20 may
        // upgrade this to a hard error or interactive disambiguation.
        let choice = if self.checked.len() == 1 {
            Some(self.checked[0])
        } else {
            None
        };
        Ratification {
            cluster_id: self.cluster_id,
            choice,
            notes: trimmed_notes.to_string(),
        }
    }
}

/// Recognize `- [x] **accept**` / `- [X] **narrow**` / etc. The bold
/// markers are tolerant — we only require the option keyword to appear
/// after the checkbox.
fn parse_checked_option(line: &str) -> Option<Choice> {
    let s = line.trim();
    if !(s.starts_with("- [x]") || s.starts_with("- [X]")) {
        return None;
    }
    // Strip the `- [x]` prefix and look for the keyword.
    let rest = &s[5..].to_lowercase();
    if rest.contains("accept") {
        Some(Choice::Accept)
    } else if rest.contains("narrow") {
        Some(Choice::Narrow)
    } else if rest.contains("reject") {
        Some(Choice::Reject)
    } else if rest.contains("bug") {
        Some(Choice::Bug)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{cluster_protos, ClusterKind, ProtoClause};

    fn proto(kind: ClusterKind, handler: &str, fid: &str) -> ProtoClause {
        ProtoClause {
            kind,
            handler: handler.to_string(),
            finding_id: fid.to_string(),
            evidence_text: String::new(),
        }
    }

    #[test]
    fn empty_clusters_produces_silent_outcome_doc() {
        let md = render_interview(&[], "test_program", "2026-05-16T00:00:00Z");
        assert!(md.contains("# Spec interview — test_program"));
        assert!(md.contains("No candidate spec clauses"));
        assert!(md.contains("## When done"));
    }

    #[test]
    fn renders_all_clusters_with_metadata() {
        let protos: Vec<_> = (1..=5)
            .map(|i| {
                proto(
                    ClusterKind::AccountOwnerCheck,
                    &format!("h{}", i),
                    &format!("f{}", i),
                )
            })
            .collect();
        let clusters = cluster_protos(protos);
        let md = render_interview(&clusters, "ptoken", "2026-05-16T00:00:00Z");
        assert!(md.contains("# Spec interview — ptoken"));
        // The promoted program-scope cluster appears under "High-confidence"
        // (5 evidence ≥ HIGH threshold).
        assert!(md.contains("## High-confidence clusters"));
        // Cluster ID appears as an HTML comment.
        assert!(md.contains("<!-- cluster: c-"), "no cluster id in {}", md);
        // Suggested syntax block rendered.
        assert!(md.contains("Suggested spec syntax"));
        // Footer rendered.
        assert!(md.contains("## When done"));
    }

    #[test]
    fn high_confidence_clusters_render_before_low() {
        let mut protos = Vec::new();
        // 5 handlers contributing AccountOwnerCheck → Program scope, High.
        for i in 1..=5 {
            protos.push(proto(
                ClusterKind::AccountOwnerCheck,
                &format!("h{}", i),
                &format!("o{}", i),
            ));
        }
        // 1 handler contributing AccountInitCheck → Handler scope, Low.
        protos.push(proto(ClusterKind::AccountInitCheck, "lonely", "i1"));
        let clusters = cluster_protos(protos);
        let md = render_interview(&clusters, "test", "2026-05-16T00:00:00Z");

        let pos_high = md.find("## High-confidence clusters").unwrap();
        let pos_low = md.find("## Low-confidence clusters").unwrap();
        assert!(
            pos_high < pos_low,
            "High band must precede Low band in the output"
        );
    }

    #[test]
    fn each_cluster_includes_four_option_checkboxes_when_program_scope() {
        let protos: Vec<_> = (1..=4)
            .map(|i| {
                proto(
                    ClusterKind::AccountOwnerCheck,
                    &format!("h{}", i),
                    &format!("f{}", i),
                )
            })
            .collect();
        let clusters = cluster_protos(protos);
        let md = render_interview(&clusters, "p", "ts");
        // Count "[ ]" occurrences in the program-scope cluster block.
        let bracket_count = md.matches("- [ ]").count();
        // Program-scope cluster shows 4 options (accept/narrow/reject/bug).
        // Footer/header options text uses bold list, no brackets, so this
        // counts only the actionable checkboxes.
        assert!(
            bracket_count >= 4,
            "expected at least 4 actionable checkboxes (got {}). MD:\n{}",
            bracket_count,
            md
        );
    }

    // ── Reader tests (M1.6) ─────────────────────────────────────────

    #[test]
    fn reader_parses_simple_accept_choice() {
        let md = "<!-- cluster: c-1234-abc -->\n- [x] **accept** — emit\n\n_notes:_\n\nlooks right\n\n---\n";
        let r = read_interview(md);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].cluster_id, "c-1234-abc");
        assert_eq!(r[0].choice, Some(Choice::Accept));
        assert_eq!(r[0].notes, "looks right");
    }

    #[test]
    fn reader_recognizes_all_four_choices() {
        let md = "
<!-- cluster: a -->
- [x] **accept**

<!-- cluster: b -->
- [x] **narrow**

<!-- cluster: c -->
- [x] **reject**

<!-- cluster: d -->
- [x] **bug**
";
        let r = read_interview(md);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].choice, Some(Choice::Accept));
        assert_eq!(r[1].choice, Some(Choice::Narrow));
        assert_eq!(r[2].choice, Some(Choice::Reject));
        assert_eq!(r[3].choice, Some(Choice::Bug));
    }

    #[test]
    fn reader_treats_unchecked_cluster_as_deferred() {
        let md = "<!-- cluster: c-x -->\n- [ ] **accept**\n- [ ] **reject**\n";
        let r = read_interview(md);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].choice, None);
    }

    #[test]
    fn reader_treats_multi_check_as_deferred() {
        let md = "<!-- cluster: c-x -->\n- [x] **accept**\n- [x] **reject**\n";
        let r = read_interview(md);
        assert_eq!(r[0].choice, None, "multi-check must NOT silently pick one");
    }

    #[test]
    fn reader_captures_multiline_notes() {
        let md = "<!-- cluster: c-x -->
- [x] **reject**

_notes:_

over-claims — only mint handlers
need this. delegate-authority case
shouldn't be flagged.

<details><summary>…</summary>
";
        let r = read_interview(md);
        assert!(r[0].notes.contains("over-claims"));
        assert!(r[0].notes.contains("delegate-authority"));
        // Must terminate the notes block at the `<details>` boundary.
        assert!(!r[0].notes.contains("<details>"));
    }

    #[test]
    fn reader_round_trips_full_interview_against_writer() {
        // Render an interview, simulate the user's edit (check `accept`
        // on every cluster), then parse — every cluster must come back
        // with Choice::Accept.
        let protos: Vec<_> = (1..=5)
            .map(|i| {
                proto(
                    ClusterKind::AccountOwnerCheck,
                    &format!("h{}", i),
                    &format!("f{}", i),
                )
            })
            .collect();
        let clusters = cluster_protos(protos);
        let rendered = render_interview(&clusters, "p", "2026-05-16T00:00:00Z");
        // Replace every `[ ] **accept**` with `[x] **accept**` to
        // simulate the user accepting all clusters.
        let edited = rendered.replace("[ ] **accept**", "[x] **accept**");
        let ratifications = read_interview(&edited);
        assert_eq!(ratifications.len(), clusters.len());
        for r in &ratifications {
            assert_eq!(r.choice, Some(Choice::Accept), "cluster {}", r.cluster_id);
        }
    }

    #[test]
    fn reader_uppercase_x_also_recognized() {
        let md = "<!-- cluster: c-x -->\n- [X] **accept**\n";
        let r = read_interview(md);
        assert_eq!(r[0].choice, Some(Choice::Accept));
    }

    #[test]
    fn reader_ignores_unanchored_content_before_first_cluster() {
        let md = "# Header\n\n- [x] **accept**\n\n<!-- cluster: c-real -->\n- [x] **reject**\n";
        let r = read_interview(md);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].cluster_id, "c-real");
        assert_eq!(r[0].choice, Some(Choice::Reject));
    }

    #[test]
    fn output_is_pure_function_of_inputs() {
        let protos: Vec<_> = (1..=3)
            .map(|i| {
                proto(
                    ClusterKind::AccountInitCheck,
                    &format!("h{}", i),
                    &format!("f{}", i),
                )
            })
            .collect();
        let c1 = cluster_protos(protos.clone());
        let c2 = cluster_protos(protos);
        let md1 = render_interview(&c1, "p", "2026-05-16T00:00:00Z");
        let md2 = render_interview(&c2, "p", "2026-05-16T00:00:00Z");
        assert_eq!(md1, md2, "interview rendering must be deterministic");
    }
}
