//! The `qedgen lift` report and its axiom gate.

use serde::Serialize;
use std::fmt::Write;

const STD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const NATIVE_DECIDE: &str = "._native.native_decide.ax_";

/// The standard axioms, plus the per-use axioms `native_decide` adds to the generated
/// check theorems in `checks` (qedsvm#98). An axiom that only looks like one is refused.
pub fn axiom_allowed(name: &str, checks: &[String]) -> bool {
    STD_AXIOMS.contains(&name)
        || checks
            .iter()
            .any(|c| name.starts_with(&format!("{c}{NATIVE_DECIDE}")))
}

/// The generated theorems that may use `native_decide`.
pub fn native_check_decls(prefix: &str, bindings: &serde_json::Value) -> Vec<String> {
    let mut decls = vec![
        format!("Examples.{prefix}.decodes"),
        format!("Examples.{prefix}.noRetWitness_ok"),
    ];
    for e in bindings["edges"].as_array().into_iter().flatten() {
        if let Some(pc) = e["pc"].as_u64() {
            for check in ["check", "absOk", "pins"] {
                decls.push(format!("Examples.{prefix}.{check}{pc}"));
            }
        }
    }
    decls
}

/// `(theorem, axioms)` from Lean's `#print axioms` output.
pub fn parse_axioms(out: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    let mut rest = out;
    while let Some(start) = rest.find('\'') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('\'') else { break };
        let name = after[..end].to_string();
        let tail = &after[end + 1..];
        if let Some(body) = tail.strip_prefix(" depends on axioms: [") {
            let Some(close) = body.find(']') else { break };
            let axioms = body[..close]
                .split(',')
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect();
            found.push((name, axioms));
            rest = &body[close + 1..];
        } else if let Some(body) = tail.strip_prefix(" does not depend on any axioms") {
            found.push((name, Vec::new()));
            rest = body;
        } else {
            rest = tail;
        }
    }
    found
}

#[derive(Debug, Serialize)]
pub struct AxiomSummary {
    /// Distinct `native_decide` axioms the theorems depend on.
    pub native_decide: usize,
    pub disallowed: Vec<String>,
    /// Theorems Lean printed no axioms for (not found or not checked).
    pub missing: Vec<String>,
}

impl AxiomSummary {
    pub fn of(theorems: &[String], printed: &[(String, Vec<String>)], checks: &[String]) -> Self {
        let mut native = std::collections::BTreeSet::new();
        let mut disallowed = std::collections::BTreeSet::new();
        let mut missing = Vec::new();
        for t in theorems {
            match printed.iter().find(|(n, _)| n == t) {
                None => missing.push(t.clone()),
                Some((_, axioms)) => {
                    for a in axioms {
                        if !axiom_allowed(a, checks) {
                            disallowed.insert(a.clone());
                        } else if a.contains(NATIVE_DECIDE) {
                            native.insert(a.clone());
                        }
                    }
                }
            }
        }
        Self {
            native_decide: native.len(),
            disallowed: disallowed.into_iter().collect(),
            missing,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InstructionReport {
    pub discriminator: u64,
    pub handler: Option<String>,
    pub success_edges: Vec<u64>,
    pub signers: Vec<u64>,
    pub key_compares: Vec<String>,
}

impl InstructionReport {
    /// Handler labels and edges from the bindings; signers and key compares only as the
    /// generated theorems state them (`coverage.route_facts`), never from discovery alone.
    pub fn of(bindings: &serde_json::Value, coverage: &serde_json::Value) -> Vec<Self> {
        let facts: Vec<&serde_json::Value> = coverage["route_facts"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        let nums = |v: &serde_json::Value| -> Vec<u64> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_u64())
                .collect()
        };
        bindings["instructions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|ix| {
                let disc = ix["discriminator"].as_u64().unwrap_or(0);
                let mine: Vec<_> = facts
                    .iter()
                    .filter(|r| r["discriminator"].as_u64() == Some(disc))
                    .collect();
                let mut signers: Vec<u64> = mine
                    .iter()
                    .flat_map(|r| nums(&r["proven_signers"]))
                    .collect();
                signers.sort_unstable();
                signers.dedup();
                let mut key_compares: Vec<String> = mine
                    .iter()
                    .flat_map(|r| r["proven_key_compares"].as_array().into_iter().flatten())
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
                key_compares.sort();
                key_compares.dedup();
                InstructionReport {
                    discriminator: disc,
                    handler: ix["handler"].as_str().map(str::to_string),
                    success_edges: nums(&ix["success_edges"]),
                    signers,
                    key_compares,
                }
            })
            .collect()
    }
}

/// What the theorems do not cover, stated plainly.
pub fn gaps(bindings: &serde_json::Value, coverage: &serde_json::Value) -> Vec<String> {
    let mut gaps = vec![
        "The bytecode checks use native_decide, which adds one axiom per use; qedsvm#98 tracks kernel-checked replacements.".to_string(),
        "Only guards in the entry function count; handlers must be inlined into the entrypoint.".to_string(),
        "No concrete run yet shows that the theorems' hypotheses are satisfiable; a wrong assumed length would make a theorem vacuous.".to_string(),
        "A loop anywhere on the way to a shared success edge marks every instruction on that edge unsupported.".to_string(),
    ];
    for l in coverage["assumed_lengths"].as_array().into_iter().flatten() {
        if l["checked"] == true {
            gaps.push(format!(
                "Edge {}: the theorem covers only inputs where account {} has {} data bytes (the program also checks this).",
                l["edge"], l["account"], l["value"]
            ));
        }
    }
    for r in coverage["route_facts"].as_array().into_iter().flatten() {
        for u in r["unpinned"].as_array().into_iter().flatten() {
            gaps.push(format!(
                "Instruction {}: the guard `{}` at {} is not stated (its operands are not pinned).",
                r["discriminator"],
                u["insn"].as_str().unwrap_or(""),
                u["pc"]
            ));
        }
        for q in r["opaque_calls"].as_array().into_iter().flatten() {
            gaps.push(format!(
                "Instruction {}: the route calls a function at {q}; guards inside it are not seen.",
                r["discriminator"]
            ));
        }
    }
    for e in bindings["edges"].as_array().into_iter().flatten() {
        for (n, w) in e["assumed"].as_array().into_iter().flatten().enumerate() {
            if w["checked"] == false {
                gaps.push(format!(
                    "Edge {}: account {n}'s data length is assumed to be {}; the program does not check it.",
                    e["pc"], w["value"]
                ));
            }
        }
        for f in e["fields"].as_array().into_iter().flatten() {
            if let Some(why) = f["refused"].as_str() {
                gaps.push(format!(
                    "Edge {}: account {} data offset {} has no field label: {why}.",
                    e["pc"], f["account"], f["data_offset"]
                ));
            }
        }
    }
    for u in bindings["unsupported"].as_array().into_iter().flatten() {
        gaps.push(format!(
            "Success edge {} is not covered: {}.",
            u["pc"],
            u["reason"].as_str().unwrap_or("")
        ));
    }
    gaps
}

#[derive(Debug, Serialize)]
pub struct LiftReport {
    pub schema_version: u32,
    pub library: String,
    pub program_hash: String,
    pub commit: String,
    pub qedsvm_rev: String,
    pub passed: bool,
    pub coverage: serde_json::Value,
    pub instructions: Vec<InstructionReport>,
    pub theorems: Vec<String>,
    pub axioms: AxiomSummary,
    pub gaps: Vec<String>,
}

impl LiftReport {
    pub fn text(&self) -> String {
        let mut s = String::new();
        let verdict = if self.passed { "PASSED" } else { "FAILED" };
        writeln!(s, "qedgen lift: {} ({verdict})", self.library).unwrap();
        writeln!(
            s,
            "  program hash {}, commit {}",
            self.program_hash, self.commit
        )
        .unwrap();
        writeln!(s, "  qedsvm {}", self.qedsvm_rev).unwrap();
        writeln!(s, "\nInstructions:").unwrap();
        for ix in &self.instructions {
            writeln!(
                s,
                "  {} {}: success edges {:?}; signers {:?}{}",
                ix.discriminator,
                ix.handler.as_deref().unwrap_or("(no label)"),
                ix.success_edges,
                ix.signers,
                if ix.key_compares.is_empty() {
                    String::new()
                } else {
                    format!("; {}", ix.key_compares.join(", "))
                }
            )
            .unwrap();
        }
        writeln!(s, "\nCoverage: {}", self.coverage).unwrap();
        writeln!(s, "\nTheorems: {}", self.theorems.join(", ")).unwrap();
        writeln!(
            s,
            "Axioms: standard three plus {} native_decide axioms; disallowed {:?}; missing {:?}",
            self.axioms.native_decide, self.axioms.disallowed, self.axioms.missing
        )
        .unwrap();
        writeln!(s, "\nGaps:").unwrap();
        for g in &self.gaps {
            writeln!(s, "  - {g}").unwrap();
        }
        s
    }
}
