//! sBPF assembly → Lean 4 transpiler
//!
//! Parses `.s` files (`.equ` constants, labels, instructions) and emits
//! a Lean 4 module with `abbrev` constants and `@[simp] def prog`.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
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

/// A `.rodata` symbol: label name, byte offset within the rodata blob, and
/// its content bytes (accumulated across directives until the next label).
#[derive(Debug, Clone)]
struct RodataSymbol {
    name: String,
    offset: usize,
    bytes: Vec<u8>,
}

struct ParsedProgram {
    equates: Vec<(String, i64)>,     // insertion-order
    equates_hex: HashSet<String>,    // names originally written in hex
    offset_symbols: HashSet<String>, // symbols used as memory offsets → typed Int
    instructions: Vec<AsmInsn>,
    labels: HashMap<String, usize>, // label → instruction index
    rodata: Vec<RodataSymbol>,      // insertion-order (= layout order)
    warnings: Vec<String>,
}

/// Lean constant name for a `.rodata` symbol (`e` → `RODATA_e`).
fn rodata_lean_name(sym: &str) -> String {
    let sanitized: String = sym
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("RODATA_{}", sanitized)
}

/// Parse a string literal starting at the first `"` in `s`. Returns the
/// decoded bytes. Supports the common GAS escapes.
fn parse_string_literal(s: &str, line_no: usize) -> Result<Vec<u8>> {
    let start = s
        .find('"')
        .with_context(|| format!("line {}: expected string literal", line_no))?;
    let mut bytes = Vec::new();
    let mut chars = s[start + 1..].chars();
    loop {
        let c = chars
            .next()
            .with_context(|| format!("line {}: unterminated string literal", line_no))?;
        match c {
            '"' => return Ok(bytes),
            '\\' => {
                let esc = chars
                    .next()
                    .with_context(|| format!("line {}: dangling escape", line_no))?;
                match esc {
                    'n' => bytes.push(b'\n'),
                    't' => bytes.push(b'\t'),
                    'r' => bytes.push(b'\r'),
                    '0' => bytes.push(0),
                    '\\' => bytes.push(b'\\'),
                    '"' => bytes.push(b'"'),
                    'x' => {
                        let h1 = chars.next().and_then(|c| c.to_digit(16));
                        let h2 = chars.next().and_then(|c| c.to_digit(16));
                        match (h1, h2) {
                            (Some(a), Some(b)) => bytes.push((a * 16 + b) as u8),
                            _ => bail!("line {}: bad \\x escape", line_no),
                        }
                    }
                    other => bail!("line {}: unsupported escape '\\{}'", line_no, other),
                }
            }
            other => {
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
}

/// Parse one numeric argument (decimal or 0x hex, optionally negative).
fn parse_rodata_num(s: &str, line_no: usize) -> Result<i64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, s),
    };
    let v = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16)
    } else {
        body.parse::<i64>()
    }
    .with_context(|| format!("line {}: bad numeric literal '{}'", line_no, s))?;
    Ok(if neg { -v } else { v })
}

/// Decode one `.rodata` data directive into bytes (little-endian for the
/// multi-byte widths). Returns `None` (with a warning) for directives we
/// don't lay out — the caller must then treat subsequent offsets as unknown.
fn parse_rodata_directive(
    line: &str,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<Vec<u8>>> {
    let (dir, args) = match line.find(|c: char| c.is_whitespace()) {
        Some(pos) => (&line[..pos], line[pos..].trim()),
        None => (line, ""),
    };
    let width: usize = match dir {
        ".ascii" => return parse_string_literal(args, line_no).map(Some),
        ".asciz" | ".string" => {
            let mut b = parse_string_literal(args, line_no)?;
            b.push(0);
            return Ok(Some(b));
        }
        ".byte" => 1,
        ".short" | ".2byte" | ".half" => 2,
        ".word" | ".long" | ".4byte" => 4,
        ".quad" | ".8byte" | ".dword" => 8,
        other => {
            warnings.push(format!(
                "line {}: unsupported .rodata directive '{}' — subsequent rodata offsets are unknown",
                line_no, other
            ));
            return Ok(None);
        }
    };
    let mut bytes = Vec::new();
    for arg in args.split(',') {
        let v = parse_rodata_num(arg, line_no)? as u64;
        bytes.extend_from_slice(&v.to_le_bytes()[..width]);
    }
    Ok(Some(bytes))
}

/// State threaded through `.rodata` line parsing.
#[derive(Default)]
struct RodataState {
    symbols: Vec<RodataSymbol>,
    offset: usize,
    /// A directive we can't lay out poisons every later offset.
    layout_broken: bool,
}

fn parse_rodata_line(
    line: &str,
    line_no: usize,
    st: &mut RodataState,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut rest = line;
    if let Some(colon) = rest.find(':') {
        let before = rest[..colon].trim();
        if !before.is_empty()
            && !before.contains(' ')
            && !before.contains('"')
            && !before.starts_with('.')
        {
            if st.layout_broken {
                warnings.push(format!(
                    "line {}: rodata symbol '{}' follows an un-layoutable directive — resolving to 0",
                    line_no, before
                ));
            } else {
                st.symbols.push(RodataSymbol {
                    name: before.to_string(),
                    offset: st.offset,
                    bytes: Vec::new(),
                });
            }
            rest = rest[colon + 1..].trim();
        }
    }
    if rest.is_empty() {
        return Ok(());
    }
    match parse_rodata_directive(rest, line_no, warnings)? {
        Some(bytes) => {
            st.offset += bytes.len();
            if let Some(last) = st.symbols.last_mut() {
                last.bytes.extend(bytes);
            } else {
                warnings.push(format!(
                    "line {}: .rodata bytes before any label — laid out but unnameable",
                    line_no
                ));
            }
        }
        None => st.layout_broken = true,
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn strip_comment(line: &str) -> &str {
    // Strip `//` comments, and `#` comments outside brackets. Both markers
    // are ignored inside string literals (`.ascii "a # b"` keeps its hash).
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut escaped = false;
    let mut bracket = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return line[..i].trim(),
            b'#' if bracket <= 0 => return line[..i].trim(),
            _ => {}
        }
        i += 1;
    }
    line.trim()
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
    // Symbol — keep as Sym so codegen emits named constants.
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
    let mut rodata_state = RodataState::default();

    // ── Pass 1: collect .equ, labels, .rodata, and raw instruction lines ──
    {
        let mut in_rodata = false;
        let mut pending_label: Option<String> = None;
        let mut insn_count: usize = 0;

        for (line_no, raw_line) in source.lines().enumerate() {
            let line = strip_comment(raw_line);
            if line.is_empty() {
                continue;
            }
            if line.starts_with(".rodata")
                || (line.starts_with(".section") && line.contains(".rodata"))
            {
                in_rodata = true;
                continue;
            }
            if line == ".text" || (line.starts_with(".section") && line.contains(".text")) {
                in_rodata = false;
                continue;
            }
            if in_rodata {
                parse_rodata_line(line, line_no + 1, &mut rodata_state, &mut warnings)?;
                continue;
            }

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

            if line.starts_with(".globl") || line.starts_with(".global") {
                continue;
            }

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
    let rodata_names: HashSet<&str> = rodata_state
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    for insn in &instructions {
        if insn.mnemonic == "call" {
            continue; // operand is a syscall name, not a symbol
        }
        for op in &insn.operands {
            if let Operand::Imm(Value::Sym(s))
            | Operand::Mem(_, Value::Sym(s))
            | Operand::Imm(Value::NegSym(s))
            | Operand::Mem(_, Value::NegSym(s)) = op
            {
                if !equates_map.contains_key(s)
                    && !labels.contains_key(s)
                    && !rodata_names.contains(s.as_str())
                {
                    warnings.push(format!(
                        "line {}: undefined symbol '{}', using 0",
                        insn.line_no, s
                    ));
                }
            }
        }
    }

    Ok(ParsedProgram {
        equates: equates_ordered,
        equates_hex,
        offset_symbols,
        instructions,
        labels,
        rodata: rodata_state.symbols,
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
fn lean_value(
    v: &Value,
    equates: &HashMap<String, i64>,
    rodata: &HashMap<String, String>,
) -> String {
    match v {
        Value::Num(n) => format_num(*n),
        Value::Sym(s) => {
            if equates.contains_key(s) {
                s.clone()
            } else if let Some(name) = rodata.get(s) {
                name.clone()
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

fn format_num(n: i64) -> String {
    if n < 0 {
        format!("({})", n)
    } else if n > 255 {
        format!("0x{:04x}", n)
    } else {
        format!("{}", n)
    }
}

fn lean_src(
    op: &Operand,
    equates: &HashMap<String, i64>,
    labels: &HashMap<String, usize>,
    rodata: &HashMap<String, String>,
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
            format!("(.imm {})", lean_value(v, equates, rodata))
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
    rodata: &HashMap<String, String>,
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
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates, rodata)),
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
            Operand::Imm(v) => lean_value(v, equates, rodata),
            _ => bail!("line {}: lddw src must be immediate", insn.line_no),
        };
        return Ok(format!(".lddw {} {}", dst, val));
    }

    // Store instructions
    if mn.starts_with("stx") {
        // stx{b,h,w,dw} [dst + off], src
        let width = lean_width(mn);
        let (dst, off) = match &ops[0] {
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates, rodata)),
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
            Operand::Mem(base, offset) => (lean_reg(base), lean_value(offset, equates, rodata)),
            _ => bail!("line {}: st dst must be memory operand", insn.line_no),
        };
        let imm = match &ops[1] {
            // st takes imm : Nat, so use imm_value for proper type handling
            Operand::Imm(v) => lean_value(v, equates, rodata),
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
        let src = lean_src(&ops[1], equates, labels, rodata);
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
        let src = lean_src(&ops[1], equates, labels, rodata);
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

/// Compute the SHA-256 hash of a source string.
pub fn source_hash(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extract the source hash embedded in a generated Lean file, if present.
pub fn extract_source_hash(lean_content: &str) -> Option<String> {
    for line in lean_content.lines() {
        if let Some(rest) = line.strip_prefix("-- source-hash: sha256:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

pub fn generate(source: &str, namespace: &str, input_filename: &str) -> Result<String> {
    let prog = parse(source)?;
    let equates_map: HashMap<String, i64> = prog.equates.iter().cloned().collect();
    // .rodata layout: the program region base (BYTECODE_START = 0x100000000,
    // where the loader's R_BPF_64_Relative patching lands sub-region
    // addresses) + the .text size in binary slots (lddw occupies 2).
    let text_slots: usize = prog
        .instructions
        .iter()
        .map(|i| if i.mnemonic == "lddw" { 2 } else { 1 })
        .sum();
    let rodata_base: u64 = 0x1_0000_0000u64 + (text_slots as u64) * 8;
    let rodata_names: HashMap<String, String> = prog
        .rodata
        .iter()
        .map(|s| (s.name.clone(), rodata_lean_name(&s.name)))
        .collect();

    for w in &prog.warnings {
        eprintln!("warning: {}", w);
    }

    let hash = source_hash(source);

    let mut out = String::new();

    writeln!(
        out,
        "-- Auto-generated by qedgen asm2lean from {}",
        input_filename
    )?;
    writeln!(
        out,
        "-- DO NOT EDIT — regenerate with: qedgen asm2lean --input {}",
        input_filename
    )?;
    writeln!(out, "-- source-hash: sha256:{}\n", hash)?;
    writeln!(out, "import SVM.SBPF\n")?;
    writeln!(out, "namespace {}\n", namespace)?;
    writeln!(out, "open SVM.SBPF\n")?;

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

    if !prog.rodata.is_empty() {
        writeln!(out, "/-! ## .rodata symbols\n")?;
        writeln!(
            out,
            "Laid out in the program region: `BYTECODE_START` (0x100000000) + .text size"
        )?;
        writeln!(
            out,
            "({} binary slots × 8 bytes; `lddw` occupies 2 slots). Deployed VAs additionally",
            text_slots
        )?;
        writeln!(
            out,
            "include ELF header/section offsets a source-level lift cannot see, so proofs"
        )?;
        writeln!(
            out,
            "MUST reference these symbols by name — a corrected base only shifts the"
        )?;
        writeln!(
            out,
            "numerals. Fidelity to the deployed binary is the binary lane's job (qedlift). -/\n"
        )?;
        for sym in &prog.rodata {
            let name = &rodata_names[&sym.name];
            let printable =
                sym.bytes.iter().all(|&b| (0x20..0x7f).contains(&b)) && !sym.bytes.is_empty();
            let preview = if printable {
                format!("\"{}\"", String::from_utf8_lossy(&sym.bytes))
            } else {
                format!("{} raw bytes", sym.bytes.len())
            };
            writeln!(
                out,
                "/-- `{}` at rodata offset 0x{:x}: {} -/",
                sym.name, sym.offset, preview
            )?;
            writeln!(
                out,
                "abbrev {} : Nat := 0x{:x}",
                name,
                rodata_base + sym.offset as u64
            )?;
            writeln!(out, "abbrev {}_LEN : Nat := {}", name, sym.bytes.len())?;
            let byte_list = sym
                .bytes
                .iter()
                .map(|b| format!("0x{:02x}", b))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "def {}_BYTES : ByteArray := ⟨#[{}]⟩", name, byte_list)?;
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
            writeln!(out, "open SVM.SBPF.Memory\n")?;
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
                        } else if let Some(name) = rodata_names.get(s.as_str()) {
                            // rodata addresses are Nat abbrevs loaded via lddw
                            if !lddw_nat_syms.contains(name) {
                                lddw_nat_syms.push(name.clone());
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

    // Render every instruction once; the chunked/flat progAt emitters, the
    // prog array, and the fetch cache all reuse the same rendered forms.
    let rendered_insns: Vec<String> = prog
        .instructions
        .iter()
        .map(|insn| emit_insn(insn, &equates_map, &prog.labels, &rodata_names))
        .collect::<Result<_>>()?;

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

        for chunk_idx in 0..n_chunks {
            let start = chunk_idx * CHUNK_SIZE;
            let end = std::cmp::min(start + CHUNK_SIZE, n_insns);

            writeln!(out, "def progAt_{} : Nat → Option SVM.SBPF.Insn", chunk_idx)?;

            for (idx, (insn, lean)) in prog
                .instructions
                .iter()
                .zip(rendered_insns.iter())
                .enumerate()
                .take(end)
                .skip(start)
            {
                let comment = if let Some(ref lbl) = insn.label {
                    format!("-- {}: {}", idx, lbl)
                } else {
                    format!("-- {}", idx)
                };
                writeln!(out, "  | {} => some ({})  {}", idx, lean, comment)?;
            }
            writeln!(out, "  | _ => none\n")?;
        }

        writeln!(out, "def progAt (n : Nat) : Option SVM.SBPF.Insn :=")?;
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
        for (idx, lean) in rendered_insns.iter().enumerate() {
            let comma = if idx + 1 < n_insns { "," } else { "" };
            writeln!(out, "  {}{}", lean, comma)?;
        }
        writeln!(out, "]\n")?;
    } else {
        writeln!(out, "/-! ## Program -/\n")?;
        writeln!(out, "@[simp] def prog : Program := #[")?;

        for (idx, insn) in prog.instructions.iter().enumerate() {
            let lean = &rendered_insns[idx];
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

        // Flat match-based fetch — the fetch-cache theorems below and the
        // wp_exec dsimp lists in proof files both reference `progAt`.
        writeln!(out, "@[simp] def progAt : Nat → Option SVM.SBPF.Insn")?;
        for (idx, _insn) in prog.instructions.iter().enumerate() {
            let lean = &rendered_insns[idx];
            writeln!(out, "  | {} => some ({})", idx, lean)?;
        }
        writeln!(out, "  | _ => none\n")?;
    }

    // progAt instruction fetch cache: pre-computed theorems for each PC.
    // Eliminates the need for `have hfN : progAt N = some (...) := by native_decide`
    // boilerplate in proof files.
    {
        writeln!(out, "/-! ## Instruction fetch cache -/\n")?;
        for (idx, lean) in rendered_insns.iter().enumerate() {
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

    crate::codegen_shared::write_generated_file(output, &lean_code)?;

    let rodata_note = if prog.rodata.is_empty() {
        String::new()
    } else {
        format!(", {} rodata symbols", prog.rodata.len())
    };
    eprintln!(
        "✓ Generated {} ({} instructions, {} constants{})",
        output.display(),
        prog.instructions.len(),
        prog.equates.len(),
        rodata_note,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RODATA_SRC: &str = r#"
.globl entrypoint
entrypoint:
    lddw r1, e
    lddw r2, 17
    call sol_log_
    exit
.rodata
    e: .ascii "Slippage exceeded"
"#;

    #[test]
    fn rodata_symbol_parsed_with_content() {
        let prog = parse(RODATA_SRC).unwrap();
        assert_eq!(prog.rodata.len(), 1);
        assert_eq!(prog.rodata[0].name, "e");
        assert_eq!(prog.rodata[0].offset, 0);
        assert_eq!(prog.rodata[0].bytes, b"Slippage exceeded");
        assert!(prog.warnings.is_empty(), "{:?}", prog.warnings);
    }

    #[test]
    fn rodata_address_is_program_region_after_text() {
        // 2 lddw (2 slots each) + call + exit = 6 slots × 8 = 0x30
        let lean = generate(RODATA_SRC, "T", "t.s").unwrap();
        assert!(
            lean.contains("abbrev RODATA_e : Nat := 0x100000030"),
            "{}",
            lean
        );
        assert!(lean.contains("abbrev RODATA_e_LEN : Nat := 17"));
        assert!(lean.contains(
            "def RODATA_e_BYTES : ByteArray := ⟨#[0x53, 0x6c, 0x69, 0x70, 0x70, 0x61, 0x67, \
             0x65, 0x20, 0x65, 0x78, 0x63, 0x65, 0x65, 0x64, 0x65, 0x64]⟩"
        ));
    }

    #[test]
    fn rodata_symbol_resolves_in_lddw_with_bridge() {
        let lean = generate(RODATA_SRC, "T", "t.s").unwrap();
        assert!(lean.contains(".lddw .r1 RODATA_e"), "{}", lean);
        assert!(!lean.contains("undefined: e"), "{}", lean);
        assert!(lean.contains("theorem bridge_RODATA_e : toU64 (↑RODATA_e : Int) = RODATA_e"));
    }

    #[test]
    fn rodata_multi_symbol_offsets_and_widths() {
        let src = r#"
entrypoint:
    exit
.rodata
    msg: .asciz "hi"
    tbl: .byte 1, 2
         .quad 0x0102
    w:   .word 7
"#;
        let prog = parse(src).unwrap();
        let syms: Vec<(&str, usize, usize)> = prog
            .rodata
            .iter()
            .map(|s| (s.name.as_str(), s.offset, s.bytes.len()))
            .collect();
        // "hi\0" = 3 bytes; tbl = 2 + 8; w at 3 + 10
        assert_eq!(syms, vec![("msg", 0, 3), ("tbl", 3, 10), ("w", 13, 4)]);
        assert_eq!(
            prog.rodata[1].bytes,
            vec![1, 2, 0x02, 0x01, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(prog.rodata[2].bytes, vec![7, 0, 0, 0]);
    }

    #[test]
    fn rodata_string_escapes_and_comment_markers() {
        let src = "entrypoint:\n    exit\n.rodata\n    m: .ascii \"a\\n# b // c\\\"\\x41\"\n";
        let prog = parse(src).unwrap();
        assert_eq!(prog.rodata[0].bytes, b"a\n# b // c\"A");
    }

    #[test]
    fn rodata_section_directive_and_text_toggle() {
        let src = "
.section .rodata
    m: .ascii \"x\"
.section .text
entrypoint:
    lddw r1, m
    exit
";
        let prog = parse(src).unwrap();
        assert_eq!(prog.rodata.len(), 1);
        assert_eq!(prog.instructions.len(), 2);
        assert!(prog.warnings.is_empty(), "{:?}", prog.warnings);
    }

    #[test]
    fn unsupported_rodata_directive_poisons_later_symbols() {
        let src = "
entrypoint:
    lddw r1, b
    exit
.rodata
    a: .ascii \"ok\"
    .align 8
    b: .byte 1
";
        let prog = parse(src).unwrap();
        // `a` survives; `b` is dropped from layout and the lddw warns.
        assert_eq!(prog.rodata.len(), 1);
        assert_eq!(prog.rodata[0].name, "a");
        assert!(prog.warnings.iter().any(|w| w.contains(".align")));
        assert!(prog
            .warnings
            .iter()
            .any(|w| w.contains("undefined symbol 'b'")));
    }

    #[test]
    fn no_rodata_emits_no_section() {
        let src = "entrypoint:\n    exit\n";
        let lean = generate(src, "T", "t.s").unwrap();
        assert!(!lean.contains(".rodata symbols"));
        assert!(!lean.contains("RODATA_"));
    }
}
