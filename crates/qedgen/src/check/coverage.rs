use super::*;

// ============================================================================
// Coverage matrix (qedgen coverage)
// ============================================================================

/// A single cell in the operation × property coverage matrix.
#[derive(Debug, serde::Serialize)]
pub struct CoverageCell {
    pub operation: String,
    pub property: String,
    pub covered: bool,
}

/// The full coverage matrix: which operations are covered by which properties.
#[derive(Debug, serde::Serialize)]
pub struct CoverageMatrix {
    pub operations: Vec<String>,
    pub properties: Vec<String>,
    pub cells: Vec<CoverageCell>,
    pub gaps: Vec<String>,
    pub coverage_pct: f64,
}

/// Build a coverage matrix from a parsed spec.
pub fn coverage_matrix(spec: &ParsedSpec) -> CoverageMatrix {
    let op_names: Vec<String> = spec.handlers.iter().map(|o| o.name.clone()).collect();
    let prop_names: Vec<String> = spec
        .properties
        .iter()
        .filter(|p| p.expression.is_some())
        .map(|p| p.name.clone())
        .collect();

    let mut cells = Vec::new();
    let mut covered_ops = std::collections::HashSet::new();

    for op in &op_names {
        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }
            let covered = prop.preserved_by.contains(op);
            if covered {
                covered_ops.insert(op.clone());
            }
            cells.push(CoverageCell {
                operation: op.clone(),
                property: prop.name.clone(),
                covered,
            });
        }
    }

    let gaps: Vec<String> = op_names
        .iter()
        .filter(|op| !covered_ops.contains(*op))
        .cloned()
        .collect();

    let coverage_pct = if op_names.is_empty() {
        100.0
    } else {
        (covered_ops.len() as f64 / op_names.len() as f64) * 100.0
    };

    CoverageMatrix {
        operations: op_names,
        properties: prop_names,
        cells,
        gaps,
        coverage_pct,
    }
}

/// Print a formatted coverage table to stderr.
pub fn print_coverage_table(matrix: &CoverageMatrix) {
    if matrix.properties.is_empty() {
        eprintln!("No properties defined — nothing to show.");
        return;
    }

    let op_col_width = matrix
        .operations
        .iter()
        .map(|o| o.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let prop_col_width = matrix
        .properties
        .iter()
        .map(|p| p.len())
        .max()
        .unwrap_or(4)
        .max(4);

    eprint!("{:<width$}", "operation", width = op_col_width + 2);
    for prop in &matrix.properties {
        eprint!(" {:^width$}", prop, width = prop_col_width);
    }
    eprintln!();

    eprint!("{}", "-".repeat(op_col_width + 2));
    for _ in &matrix.properties {
        eprint!("-{}", "-".repeat(prop_col_width));
    }
    eprintln!();

    for op in &matrix.operations {
        eprint!("{:<width$}", op, width = op_col_width + 2);
        for prop in &matrix.properties {
            let covered = matrix
                .cells
                .iter()
                .any(|c| &c.operation == op && &c.property == prop && c.covered);
            let mark = if covered { "Y" } else { "-" };
            eprint!(" {:^width$}", mark, width = prop_col_width);
        }
        eprintln!();
    }

    eprintln!();
    eprintln!(
        "Coverage: {:.0}% ({}/{} operations covered by at least one property)",
        matrix.coverage_pct,
        matrix.operations.len() - matrix.gaps.len(),
        matrix.operations.len()
    );

    if !matrix.gaps.is_empty() {
        eprintln!("Gaps: {}", matrix.gaps.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

    // ========================================================================
    // Coverage matrix, write_without_read, circular_lifecycle
    // ========================================================================

    #[test]
    fn test_coverage_matrix_full_coverage() {
        let spec_content = include_str!("../../../../examples/rust/multisig/multisig.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("multisig.qedspec should parse");
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.coverage_pct, 100.0);
        assert!(matrix.gaps.is_empty());
        // 8 handlers: create_vault, propose, approve, reject, execute,
        // cancel_proposal, add_member, remove_member.
        assert_eq!(matrix.operations.len(), 8);
        assert_eq!(matrix.properties.len(), 2);
    }

    #[test]
    fn test_coverage_matrix_detects_gaps() {
        let mut h_covered = make_handler("deposit");
        h_covered.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        let mut h_uncovered = make_handler("withdraw");
        h_uncovered.effects = vec![ParsedEffect::from_triple("balance", "sub", "amount")];

        let spec = ParsedSpec {
            handlers: vec![h_covered, h_uncovered],
            state_fields: vec![("balance".into(), "U64".into())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                rust_expression: Some("s.balance >= 0".to_string()),
                rust_expression_pod: Some("s.balance >= 0".to_string()),
                rust_expression_math: None,
                preserved_by: vec!["deposit".to_string()], // only covers deposit
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
                tree: None,
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.gaps, vec!["withdraw"]);
        assert!(matrix.coverage_pct < 100.0);
    }
}
