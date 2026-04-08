//! sBPF assembly → Lean 4 transpiler
//!
//! Parses `.s` files (`.equ` constants, labels, instructions) and emits
//! a Lean 4 module with `abbrev` constants and `@[simp] def prog`.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A resolved value: either a raw number or a symbol name (for codegen).
#[derive(Debug, Clone)]
enum Value {
    Num(i64),
    Sym(String),
    NegSym(String), // negated symbol: -SYMBOL (for [reg - OFFSET] syntax)
}

#[derive(Debug, Clone)]
enum Operand {
    Reg(String),        // "r0" .. "r10"
    Imm(Value),         // numeric literal or symbol
    Mem(String, Value), // [base_reg + offset]
}

#[derive(Debug, Clone)]
struct AsmInsn {
    mnemonic: String,
    operands: Vec<Operand>,
    label: Option<String>, // label defined at this instruction
    line_no: usize,
}

struct ParsedProgram {
    equates: Vec<(String, i64)>,     // insertion-order
    equates_hex: HashSet<String>,    // names originally written in hex
    offset_symbols: HashSet<String>, // symbols used as memory offsets → typed Int
    instructions: Vec<AsmInsn>,
    labels: HashMap<String, usize>, // label → instruction index
    warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn strip_comment(line: &str) -> &str {
    // Handle # comments (but not inside [...])
    // Handle // comments
    let mut result = line;
    if let Some(pos) = result.find("//") {
        result = &result[..pos];
    }
    if let Some(pos) = result.find('#') {
        // Only strip if # is not inside brackets
        let before = &result[..pos];
        if before.matches('[').count() <= before.matches(']').count() {
            result = &result[..pos];
        }
    }
    result.trim()
}

fn parse_value(s: &str, _equates: &HashMap<String, i64>) -> Value {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if let Ok(v) = i64::from_str_radix(hex, 16) {
            return Value::Num(v);
        }
    }
    if let Ok(v) = s.parse::<i64>() {
        return Value::Num(v);
    }
    // It's a symbol — keep as Sym for codegen (we want named constants in the output)
    Value::Sym(s.to_string())
}

fn parse_register(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('r') {
        if let Ok(n) = rest.parse::<u32>() {
            if n <= 10 {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn is_register(s: &str) -> bool {
    parse_register(s).is_some()
}

/// Returns (base_reg, offset_str, is_negated)
fn parse_mem_operand(s: &str) -> Option<(String, String, bool)> {
    let s = s.trim();
    let inner = s.strip_prefix('[')?.strip_suffix(']')?.trim();
    // Split on '+' first, then '-'
    if let Some(pos) = inner.find('+') {
        let base = inner[..pos].trim();
        let off = inner[pos + 1..].trim();
        if parse_register(base).is_some() {
            return Some((base.to_string(), off.to_string(), false));
        }
    }
    // Handle [reg - offset] (subtraction — common for stack-relative addressing)
    // Find '-' that is NOT part of a negative number at the start
    if let Some(pos) = inner.rfind('-') {
        if pos > 0 {
            let base = inner[..pos].trim();
            let off = inner[pos + 1..].trim();
            if parse_register(base).is_some() && !off.is_empty() {
                return Some((base.to_string(), off.to_string(), true));
            }
        }
    }
    // No offset — just [reg]
    if parse_register(inner).is_some() {
        return Some((inner.to_string(), "0".to_string(), false));
    }
    None
}

fn parse_operands(rest: &str, equates: &HashMap<String, i64>) -> Vec<Operand> {
    if rest.is_empty() {
        return vec![];
    }

    let mut operands = Vec::new();
    let mut current = String::new();
    let mut bracket_depth = 0;

    for ch in rest.chars() {
        match ch {
            '[' => {
                bracket_depth += 1;
                current.push(ch);
            }
            ']' => {
                bracket_depth -= 1;
                current.push(ch);
            }
            ',' if bracket_depth == 0 => {
                let token = current.trim().to_string();
                if !token.is_empty() {
                    operands.push(token);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let token = current.trim().to_string();
    if !token.is_empty() {
        operands.push(token);
    }

    operands
        .iter()
        .map(|tok| {
            let tok = tok.trim();
            if let Some((base, off, negated)) = parse_mem_operand(tok) {
                let val = parse_value(&off, equates);
                let val = if negated {
                    match val {
                        Value::Num(n) => Value::Num(-n),
                        Value::Sym(s) => Value::NegSym(s),
                        Value::NegSym(s) => Value::Sym(s), // double negation
                    }
                } else {
                    val
                };
                Operand::Mem(base, val)
            } else if is_register(tok) {
                Operand::Reg(tok.to_string())
            } else {
                Operand::Imm(parse_value(tok, equates))
            }
        })
        .collect()
}

fn parse(source: &str) -> Result<ParsedProgram> {
    let mut equates_map: HashMap<String, i64> = HashMap::new();
    let mut equates_ordered: Vec<(String, i64)> = Vec::new();
    let mut equates_hex: HashSet<String> = HashSet::new();
    let mut offset_symbols: HashSet<String> = HashSet::new();
    let mut labels: HashMap<String, usize> = HashMap::new();
    let mut instructions: Vec<AsmInsn> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Intermediate: raw instruction lines (mnemonic, operand_text, line_no)
    let mut raw_insns: Vec<(String, String, usize)> = Vec::new();

    // ── Pass 1: collect .equ, labels, and raw instruction lines ──
    {
        let mut in_rodata = false;
        let mut pending_label: Option<String> = None;
        let mut insn_count: usize = 0;

        for (line_no, raw_line) in source.lines().enumerate() {
            let line = strip_comment(raw_line);
            if line.is_empty() {
                continue;
            }
            if line.starts_with(".rodata") {
                in_rodata = true;
                continue;
            }
            if in_rodata {
                continue;
            }

            // .equ directive
            if line.starts_with(".equ ") || line.starts_with(".equ\t") {
                let rest = line[5..].trim();
                if let Some(comma_pos) = rest.find(',') {
                    let name = rest[..comma_pos].trim().to_string();
                    let val_str = rest[comma_pos + 1..].trim();
                    let is_hex = val_str.starts_with("0x") || val_str.starts_with("0X");
                    let val = if let Some(hex) = val_str
                        .strip_prefix("0x")
                        .or_else(|| val_str.strip_prefix("0X"))
                    {
                        i64::from_str_radix(hex, 16)
                            .with_context(|| format!("line {}: bad hex in .equ", line_no + 1))?
                    } else {
                        val_str
                            .parse::<i64>()
                            .with_context(|| format!("line {}: bad value in .equ", line_no + 1))?
                    };
                    equates_map.insert(name.clone(), val);
                    equates_ordered.push((name.clone(), val));
                    if is_hex {
                        equates_hex.insert(name);
                    }
                }
                continue;
            }

            // .globl / .global
            if line.starts_with(".globl") || line.starts_with(".global") {
                continue;
            }

            // Label
            if let Some(colon_pos) = line.find(':') {
                let before = line[..colon_pos].trim();
                if !before.is_empty()
                    && !before.contains(' ')
                    && !before.contains('[')
                    && !before.starts_with('.')
                {
                    let label_name = before.to_string();
                    // Flush previous pending label (consecutive labels map to same index)
                    if let Some(prev_lbl) = pending_label.take() {
                        labels.insert(prev_lbl, insn_count);
                    }
                    pending_label = Some(label_name);
                    let after = line[colon_pos + 1..].trim();
                    if after.is_empty() {
                        continue;
                    }
                    // Instruction on same line as label
                    if let Some(lbl) = pending_label.take() {
                        labels.insert(lbl, insn_count);
                    }
                    let (mnemonic, rest) = match after.find(|c: char| c.is_whitespace()) {
                        Some(pos) => (after[..pos].to_string(), after[pos..].trim().to_string()),
                        None => (after.to_string(), String::new()),
                    };
                    raw_insns.push((mnemonic, rest, line_no + 1));
                    insn_count += 1;
                    continue;
                }
            }

            // Instruction
            if let Some(lbl) = pending_label.take() {
                labels.insert(lbl, insn_count);
            }
            let line_trimmed = line.trim();
            let (mnemonic, rest) = match line_trimmed.find(|c: char| c.is_whitespace()) {
                Some(pos) => (
                    line_trimmed[..pos].to_string(),
                    line_trimmed[pos..].trim().to_string(),
                ),
                None => (line_trimmed.to_string(), String::new()),
            };
            raw_insns.push((mnemonic, rest, line_no + 1));
            insn_count += 1;
        }

        if let Some(lbl) = pending_label {
            labels.insert(lbl, insn_count);
        }
    }

    // ── Pass 2: parse operands with full label + equate maps ──
    for (mnemonic, rest, line_no) in &raw_insns {
        let operands = parse_operands(rest, &equates_map);
        for op in &operands {
            match op {
                Operand::Mem(_, Value::Sym(s)) | Operand::Mem(_, Value::NegSym(s)) => {
                    offset_symbols.insert(s.clone());
                }
                _ => {}
            }
        }
        instructions.push(AsmInsn {
            mnemonic: mnemonic.clone(),
            operands,
            label: None,
            line_no: *line_no,
        });
    }

    // Annotate instructions with their labels
    let label_at_idx: HashMap<usize, String> = labels
        .iter()
        .map(|(name, &idx)| (idx, name.clone()))
        .collect();
    for (idx, insn) in instructions.iter_mut().enumerate() {
        if let Some(lbl) = label_at_idx.get(&idx) {
            insn.label = Some(lbl.clone());
        }
    }

    // Check for undefined symbols used in instructions (skip syscall names in `call`)
    for insn in &instructions {
        if insn.mnemonic == "call" {
            continue; // operand is a syscall name, not a symbol
        }
        for op in &insn.operands {
            match op {
                Operand::Imm(Value::Sym(s))
                | Operand::Mem(_, Value::Sym(s))
                | Operand::Imm(Value::NegSym(s))
                | Operand::Mem(_, Value::NegSym(s)) => {
                    if !equates_map.contains_key(s) && !labels.contains_key(s) {
                        warnings.push(format!(
                            "line {}: undefined symbol '{}', using 0",
                            insn.line_no, s
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    Ok(ParsedProgram {
        equates: equates_ordered,
        equates_hex,
        offset_symbols,
        instructions,
        labels,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Lean code generation
// ---------------------------------------------------------------------------

fn lean_reg(r: &str) -> String {
    format!(".{}", r)
}

fn lean_width(mnemonic: &str) -> &'static str {
    if mnemonic.ends_with("dw") {
        ".dword"
    } else if mnemonic.ends_with('w') {
        ".word"
    } else if mnemonic.ends_with('h') {
        ".half"
    } else if mnemonic.ends_with('b') {
        ".byte"
    } else {
        ".dword" // default for lddw
    }
}

/// Format a value for use in the prog array — use named constants when available
/// so that `simp` can match hypothesis terms syntactically (avoids abbrev unfolding).
fn lean_value(v: &Value, equates: &HashMap<String, i64>) -> String {
    match v {
        Value::Num(n) => format_num(*n),
        Value::Sym(s) => {
            if equates.contains_key(s) {
                s.clone()
            } else {
                // Undefined symbol — use 0
                format!("0 /- undefined: {} -/", s)
            }
        }
        Value::NegSym(s) => {
            if equates.contains_key(s) {
                format!("(-{})", s)
            } else {
                format!("0 /- undefined: -{} -/", s)
            }
        }
    }
}

fn format_num(n: i64) -> String {
    if n < 0 {
        format!("({})", n)
    } else if n > 255 {
        format!("0x{:04x}", n)
    } else {
        format!("{}", n)
    }
}

/// Format a value for use inside Src.imm / lddw / st (all take Int now).
/// Nat-typed constants auto-coerce to Int in Lean, so no conversion needed.
fn lean_imm_value(
    v: &Value,
    equates: &HashMap<String, i64>,
    _offset_symbols: &HashSet<String>,
) -> String {
    match v {
        Value::Num(n) => format_num(*n),
        Value::Sym(s) => {
            if equates.contains_key(s) {
                s.clone()
            } else {
                format!("0 /- undefined: {} -/", s)
            }
        }
        Value::NegSym(s) => {
            if equates.contains_key(s) {
                format!("(-{})", s)
            } else {
                format!("0 /- undefined: -{} -/", s)
            }
        }
    }
}

fn lean_src(
    op: &Operand,
    equates: &HashMap<String, i64>,
    labels: &HashMap<String, usize>,
    offset_symbols: &HashSet<String>,
) -> String {
    match op {
        Operand::Reg(r) => format!("(.reg {})", lean_reg(r)),
        Operand::Imm(v) => {
            // Check if it's a label reference (for jump targets)
            if let Value::Sym(s) = v {
                if let Some(&idx) = labels.get(s) {
                    return format!("{}", idx);
                }
            }
            format!("(.imm {})", lean_imm_value(v, equates, offset_symbols))
        }
        _ => "(.imm 0)".to_string(),
    }
}

fn lean_jump_target(
    op: &Operand,
    equates: &HashMap<String, i64>,
    labels: &HashMap<String, usize>,
) -> String {
    match op {
        Operand::Imm(Value::Sym(s)) => {
            if let Some(&idx) = labels.get(s) {
                format!("{}", idx)
            } else if let Some(&val) = equates.get(s) {
                format!("{}", val)
            } else {
                format!("0 /- WARNING: unresolved label '{}' -/", s)
            }
        }
        Operand::Imm(Value::Num(n)) => format!("{}", n),
        _ => "0".to_string(),
    }
}

fn emit_insn(
    insn: &AsmInsn,
    equates: &HashMap<String, i64>,
    labels: &HashMap<String, usize>,
    offset_symbols: &HashSet<String>,
) -> Result<String> {
    let mn = insn.mnemonic.as_str();
    let ops = &insn.operands;

    // Load instructions
    if mn.starts_with("ldx") {
        // ldx{b,h,w,dw} dst, [src + off]
        let width = lean_width(mn);
        let dst = match &ops[0] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: ldx dst must be register", insn.line_no),
        };
        let (src, off) = match &ops[1] {
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates)),
            _ => bail!("line {}: ldx src must be memory operand", insn.line_no),
        };
        return Ok(format!(".ldx {} {} {} {}", width, dst, src, off));
    }

    if mn == "lddw" {
        let dst = match &ops[0] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: lddw dst must be register", insn.line_no),
        };
        let val = match &ops[1] {
            // lddw loads a Nat, so use imm_value for proper unsigned handling
            Operand::Imm(v) => lean_imm_value(v, equates, offset_symbols),
            _ => bail!("line {}: lddw src must be immediate", insn.line_no),
        };
        return Ok(format!(".lddw {} {}", dst, val));
    }

    // Store instructions
    if mn.starts_with("stx") {
        // stx{b,h,w,dw} [dst + off], src
        let width = lean_width(mn);
        let (dst, off) = match &ops[0] {
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates)),
            _ => bail!("line {}: stx dst must be memory operand", insn.line_no),
        };
        let src = match &ops[1] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: stx src must be register", insn.line_no),
        };
        return Ok(format!(".stx {} {} {} {}", width, dst, off, src));
    }

    if mn == "st" || mn.starts_with("st") && !mn.starts_with("stx") {
        // Immediate store: st{b,h,w,dw} [dst + off], imm
        let real_mn = if mn == "st" { "stdw" } else { mn };
        let width = lean_width(real_mn);
        let (dst, off) = match &ops[0] {
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates)),
            _ => bail!("line {}: st dst must be memory operand", insn.line_no),
        };
        let imm = match &ops[1] {
            // st takes imm : Nat, so use imm_value for proper type handling
            Operand::Imm(v) => lean_imm_value(v, equates, offset_symbols),
            _ => bail!("line {}: st src must be immediate", insn.line_no),
        };
        return Ok(format!(".st {} {} {} {}", width, dst, off, imm));
    }

    // ALU instructions (binary: dst, src)
    let alu_ops = [
        "add64", "sub64", "mul64", "div64", "mod64", "or64", "and64", "xor64", "lsh64", "rsh64",
        "arsh64", "mov64", "add32", "sub32", "mul32", "div32", "mod32", "or32", "and32", "xor32",
        "lsh32", "rsh32", "arsh32", "mov32",
    ];
    if alu_ops.contains(&mn) {
        let dst = match &ops[0] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: {} dst must be register", insn.line_no, mn),
        };
        let src = lean_src(&ops[1], equates, labels, offset_symbols);
        return Ok(format!(".{} {} {}", mn, dst, src));
    }

    // neg (unary)
    if mn == "neg64" || mn == "neg32" {
        let dst = match &ops[0] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: {} dst must be register", insn.line_no, mn),
        };
        return Ok(format!(".{} {}", mn, dst));
    }

    // Conditional jumps: j{eq,ne,gt,ge,lt,le,sgt,sge,slt,sle,set} dst, src, target
    let jump_ops = [
        "jeq", "jne", "jgt", "jge", "jlt", "jle", "jsgt", "jsge", "jslt", "jsle", "jset",
    ];
    if jump_ops.contains(&mn) {
        let dst = match &ops[0] {
            Operand::Reg(r) => lean_reg(r),
            _ => bail!("line {}: {} dst must be register", insn.line_no, mn),
        };
        let src = lean_src(&ops[1], equates, labels, offset_symbols);
        let target = lean_jump_target(&ops[2], equates, labels);
        return Ok(format!(".{} {} {} {}", mn, dst, src, target));
    }

    // Unconditional jump
    if mn == "ja" {
        let target = lean_jump_target(&ops[0], equates, labels);
        return Ok(format!(".ja {}", target));
    }

    // Syscall
    if mn == "call" {
        let name = match &ops[0] {
            Operand::Imm(Value::Sym(s)) => s.clone(),
            Operand::Reg(s) => s.clone(), // parsed as "register" since sol_... doesn't match
            _ => bail!("line {}: call operand must be syscall name", insn.line_no),
        };
        // Lean syscall names have a dot prefix
        return Ok(format!(".call .{}", name));
    }

    // Exit
    if mn == "exit" {
        return Ok(".exit".to_string());
    }

    bail!("line {}: unrecognized mnemonic '{}'", insn.line_no, mn)
}

pub fn generate(source: &str, namespace: &str, input_filename: &str) -> Result<String> {
    let prog = parse(source)?;
    let equates_map: HashMap<String, i64> = prog.equates.iter().cloned().collect();

    // Print warnings
    for w in &prog.warnings {
        eprintln!("warning: {}", w);
    }

    let mut out = String::new();

    // Header
    writeln!(
        out,
        "-- Auto-generated by qedgen asm2lean from {}",
        input_filename
    )?;
    writeln!(
        out,
        "-- DO NOT EDIT — regenerate with: qedgen asm2lean --input {}\n",
        input_filename
    )?;
    writeln!(out, "import QEDGen.Solana.SBPF\n")?;
    writeln!(out, "namespace {}\n", namespace)?;
    writeln!(out, "open QEDGen.Solana.SBPF\n")?;

    // .equ constants
    if !prog.equates.is_empty() {
        writeln!(out, "/-! ## .equ constants -/\n")?;
        for (name, val) in &prog.equates {
            // Int if: used as memory offset, OR negative value.
            // Nat otherwise (non-negative immediates, error codes, etc.)
            let ty = if prog.offset_symbols.contains(name) || *val < 0 {
                "Int"
            } else {
                "Nat"
            };
            if prog.equates_hex.contains(name) {
                writeln!(out, "abbrev {} : {} := 0x{:02x}", name, ty, val)?;
            } else {
                writeln!(out, "abbrev {} : {} := {}", name, ty, val)?;
            }
        }
        writeln!(out)?;
    }

    // effectiveAddr lemmas: one per offset symbol, proves effectiveAddr b OFF = b ± val
    {
        let mut offset_names: Vec<&String> = prog.offset_symbols.iter().collect();
        offset_names.sort();
        if !offset_names.is_empty() {
            writeln!(out, "/-! ## effectiveAddr lemmas -/\n")?;
            writeln!(out, "section EffectiveAddr\n")?;
            writeln!(out, "open QEDGen.Solana.SBPF.Memory\n")?;
            for name in &offset_names {
                if let Some(&val) = equates_map.get(name.as_str()) {
                    let rhs = if val == 0 {
                        "b".to_string()
                    } else if val > 0 {
                        format!("b + {}", val)
                    } else {
                        format!("b - {}", -val)
                    };
                    writeln!(
                        out,
                        "@[simp] theorem ea_{} (b : Nat) : effectiveAddr b {} = {} := by\n  unfold effectiveAddr {}; omega\n",
                        name, name, rhs, name
                    )?;
                }
            }
            writeln!(out, "end EffectiveAddr\n")?;
        }
    }

    // toU64 bridge lemmas: for Nat-typed constants used in lddw instructions.
    // lddw internally involves toU64 coercion; these bridge lemmas let simp
    // resolve `toU64 (↑NAME : Int) = NAME` without native_decide in proofs.
    {
        let mut lddw_nat_syms: Vec<String> = Vec::new();
        for insn in &prog.instructions {
            if insn.mnemonic == "lddw" {
                if let Some(Operand::Imm(Value::Sym(s))) = insn.operands.get(1) {
                    // Only emit bridge for Nat-typed constants (not offset symbols / negative)
                    if !prog.offset_symbols.contains(s) {
                        if let Some(&val) = equates_map.get(s.as_str()) {
                            if val >= 0 && !lddw_nat_syms.contains(s) {
                                lddw_nat_syms.push(s.clone());
                            }
                        }
                    }
                }
            }
        }
        if !lddw_nat_syms.is_empty() {
            lddw_nat_syms.sort();
            writeln!(out, "/-! ## toU64 bridge lemmas (lddw constants) -/\n")?;
            for name in &lddw_nat_syms {
                writeln!(
                    out,
                    "@[simp] theorem bridge_{} : toU64 (↑{} : Int) = {} := by native_decide",
                    name, name, name
                )?;
            }
            writeln!(out)?;
        }
    }

    // For large programs (>64 instructions), emit a function-based lookup
    // for O(1) simp performance. Small programs use @[simp] on the array directly.
    let use_fn_lookup = prog.instructions.len() > 64;

    if use_fn_lookup {
        writeln!(out, "/-! ## Program (chunked lookup for O(1) simp) -/\n")?;

        // Split into chunks of CHUNK_SIZE to keep each pattern match under
        // Lean's heartbeat limit.  Top-level progAt dispatches by range.
        const CHUNK_SIZE: usize = 100;
        let n_insns = prog.instructions.len();
        let n_chunks = n_insns.div_ceil(CHUNK_SIZE);

        // Emit each chunk as a private function
        for chunk_idx in 0..n_chunks {
            let start = chunk_idx * CHUNK_SIZE;
            let end = std::cmp::min(start + CHUNK_SIZE, n_insns);

            writeln!(
                out,
                "def progAt_{} : Nat → Option QEDGen.Solana.SBPF.Insn",
                chunk_idx
            )?;

            for idx in start..end {
                let insn = &prog.instructions[idx];
                let lean = emit_insn(insn, &equates_map, &prog.labels, &prog.offset_symbols)?;
                let comment = if let Some(ref lbl) = insn.label {
                    format!("-- {}: {}", idx, lbl)
                } else {
                    format!("-- {}", idx)
                };
                writeln!(out, "  | {} => some ({})  {}", idx, lean, comment)?;
            }
            writeln!(out, "  | _ => none\n")?;
        }

        // Emit top-level dispatch
        writeln!(
            out,
            "def progAt (n : Nat) : Option QEDGen.Solana.SBPF.Insn :="
        )?;
        for chunk_idx in 0..n_chunks {
            let upper = (chunk_idx + 1) * CHUNK_SIZE;
            if chunk_idx + 1 < n_chunks {
                writeln!(out, "  if n < {} then progAt_{} n", upper, chunk_idx)?;
                writeln!(out, "  else")?;
            } else {
                writeln!(out, "  progAt_{} n", chunk_idx)?;
            }
        }
        writeln!(out)?;

        writeln!(out, "def prog : Program := #[")?;
        for (idx, insn) in prog.instructions.iter().enumerate() {
            let lean = emit_insn(insn, &equates_map, &prog.labels, &prog.offset_symbols)?;
            let comma = if idx + 1 < n_insns { "," } else { "" };
            writeln!(out, "  {}{}", lean, comma)?;
        }
        writeln!(out, "]\n")?;
    } else {
        writeln!(out, "/-! ## Program -/\n")?;
        writeln!(out, "@[simp] def prog : Program := #[")?;

        for (idx, insn) in prog.instructions.iter().enumerate() {
            let lean = emit_insn(insn, &equates_map, &prog.labels, &prog.offset_symbols)?;
            let comma = if idx + 1 < prog.instructions.len() {
                ","
            } else {
                ""
            };
            let comment = if let Some(ref lbl) = insn.label {
                format!("-- {}: {}", idx, lbl)
            } else {
                format!("-- {}", idx)
            };
            writeln!(
                out,
                "  {}{:pad$}{}",
                lean,
                comma,
                comment,
                pad = 50_usize.saturating_sub(lean.len() + comma.len())
            )?;
        }

        writeln!(out, "]\n")?;
    }

    // progAt instruction fetch cache: pre-computed theorems for each PC.
    // Eliminates the need for `have hfN : progAt N = some (...) := by native_decide`
    // boilerplate in proof files.
    {
        let n_insns = prog.instructions.len();
        writeln!(out, "/-! ## Instruction fetch cache -/\n")?;
        for idx in 0..n_insns {
            let insn = &prog.instructions[idx];
            let lean = emit_insn(insn, &equates_map, &prog.labels, &prog.offset_symbols)?;
            writeln!(
                out,
                "@[simp] theorem insn_{} : progAt {} = some ({}) := by native_decide",
                idx, idx, lean
            )?;
        }
        writeln!(out)?;
    }

    writeln!(out, "end {}", namespace)?;

    Ok(out)
}

/// Entry point called from main.rs
pub fn asm2lean(input: &Path, output: &Path, namespace: Option<&str>) -> Result<()> {
    let source =
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;

    let input_filename = input
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| input.display().to_string());

    let ns = namespace.map(|s| s.to_string()).unwrap_or_else(|| {
        output
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Program".to_string())
    });

    let prog = parse(&source)?;
    let lean_code = generate(&source, &ns, &input_filename)?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, &lean_code)?;

    eprintln!(
        "✓ Generated {} ({} instructions, {} constants)",
        output.display(),
        prog.instructions.len(),
        prog.equates.len(),
    );

    Ok(())
}
