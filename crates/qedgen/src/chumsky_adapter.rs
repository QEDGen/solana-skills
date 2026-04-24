//! Adapter: typed AST (`ast::Spec`) → legacy `ParsedSpec`.
//!
//! Bridge layer that lets downstream consumers (`check.rs`, `lean_gen.rs`,
//! `kani.rs`, `proptest_gen.rs`, ...) keep reading the string-rendered
//! `ParsedSpec` while the parser produces a typed AST. Next migration step:
//! rewrite consumers against the typed AST directly, then delete this module
//! and `ParsedSpec`'s pre-rendered-string fields.
//!
//! Guard expressions are rendered to Lean-form (unicode operators, pre/post
//! state prefixes) and Rust-form (ASCII) strings here. The typed AST keeps
//! structure; the string forms are lossy projections for legacy consumers.

use crate::ast::{self as a, Expr, Node, TopItem};
use crate::check::{
    FlowKind, ParsedAccountType, ParsedCall, ParsedCallArg, ParsedCover, ParsedEnsures,
    ParsedEnvironment, ParsedErrorCode, ParsedEvent, ParsedGuard, ParsedHandler,
    ParsedHandlerAccount, ParsedInstruction, ParsedInterface, ParsedInterfaceHandler,
    ParsedLayoutField, ParsedLiveness, ParsedPda, ParsedProperty, ParsedPubkey, ParsedRecordType,
    ParsedRequires, ParsedSbpfProperty, ParsedSpec, ParsedSumType, ParsedUpstream, ParsedVariant,
    SbpfPropertyKind,
};

// ============================================================================
// Expression rendering (Lean / Rust)
// ============================================================================

#[derive(Copy, Clone)]
enum Ctx {
    /// Inside a handler's `requires` / property body / invariant —
    /// `state.X` renders with pre-state prefix.
    Guard,
    /// Inside an `ensures` clause — `state.X` is post-state `s'`, `old(X)` is pre-state `s`.
    Ensures,
}

type ConstTable<'a> = &'a std::collections::BTreeMap<String, String>;

// ----------------------------------------------------------------------------
// Type inference for mixed Nat/Int arithmetic
//
// Lean doesn't implicitly coerce Nat → Int in arithmetic. When a spec writes
// `state.accounts[i].capital + state.accounts[i].pnl` (U128 + I128 in source),
// the Lean output must wrap the Nat side as `((x : Nat) : Int)`. We resolve
// each operand's kind from a shallow type environment built during adapt().
// ----------------------------------------------------------------------------

/// Lean-level type kind for the purpose of operator coercion. We collapse
/// all unsigned widths to `Nat` and all signed widths to `Int`; `Pubkey`
/// and `Bool` propagate through equality tests but don't participate in
/// arithmetic. `Unknown` is treated as `Nat` for conservatism — the current
/// codegen already defaults to Nat on unknowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Nat,
    Int,
    Bool,
    Other,
}

/// Type environment for expression rendering.
///   - `state_fields`: bare field name → TypeRef (top-level state fields like V, I)
///   - `records`: record name → field name → TypeRef (e.g. Account.capital → U128)
///   - `params`: current handler's params, for bare-ident lookups
#[derive(Default)]
struct TypeEnv<'a> {
    state_fields: std::collections::BTreeMap<String, &'a a::TypeRef>,
    records: std::collections::BTreeMap<String, std::collections::BTreeMap<String, &'a a::TypeRef>>,
    params: Vec<(String, &'a a::TypeRef)>,
}

impl<'a> TypeEnv<'a> {
    fn from_spec(spec: &'a a::Spec) -> Self {
        let mut env = TypeEnv::default();
        for Node { node, .. } in &spec.items {
            match node {
                TopItem::Record(r) => {
                    let m: std::collections::BTreeMap<_, _> =
                        r.fields.iter().map(|f| (f.name.clone(), &f.ty)).collect();
                    env.records.insert(r.name.clone(), m);
                }
                // State-like ADTs: flatten all variant fields into the
                // state_fields map (backward-compat with the existing
                // ParsedSpec shape). The first variant carrying fields
                // wins for name collisions. `Error`-shaped ADTs are skipped.
                TopItem::Adt(a) if a.name != "Error" => {
                    for variant in &a.variants {
                        for f in &variant.fields {
                            env.state_fields.entry(f.name.clone()).or_insert(&f.ty);
                        }
                    }
                }
                _ => {}
            }
        }
        env
    }

    fn with_params(mut self, params: &'a [a::TypedField]) -> Self {
        self.params = params.iter().map(|f| (f.name.clone(), &f.ty)).collect();
        self
    }

    /// Resolve a source-language TypeRef to its Lean `Kind`.
    fn type_ref_kind(&self, t: &a::TypeRef) -> Kind {
        match t {
            a::TypeRef::Named(n) => match n.as_str() {
                "U8" | "U16" | "U32" | "U64" | "U128" => Kind::Nat,
                "I8" | "I16" | "I32" | "I64" | "I128" => Kind::Int,
                "Bool" => Kind::Bool,
                // Named records / aliases bottom out here.
                _ => Kind::Other,
            },
            a::TypeRef::Map { .. } => Kind::Other,
            a::TypeRef::Fin { .. } => Kind::Nat, // Fin n coerces to Nat for arithmetic.
            a::TypeRef::Param(_, _) => Kind::Other,
        }
    }

    /// Resolve the kind of a Path. Handles subscripts into Map fields by
    /// reading through the map's value-record to find the trailing field.
    fn path_kind(&self, p: &a::Path) -> Kind {
        // `state.x.y` or `state.accounts[i].capital` or bare `amount`
        if p.root == "state" {
            // Walk the segments: first Field must be a state field; subsequent
            // Fields index into a record or Map-of-record.
            let mut current: Option<&a::TypeRef> = None;
            for seg in &p.segments {
                match seg {
                    a::PathSeg::Field(f) => {
                        let field_ty = match current {
                            None => self.state_fields.get(f).copied(),
                            Some(a::TypeRef::Named(rec_name)) => {
                                self.records.get(rec_name).and_then(|m| m.get(f).copied())
                            }
                            Some(a::TypeRef::Map { inner, .. }) => {
                                // direct .field after a Map without [idx] shouldn't happen
                                // in valid specs, but bottom out safely
                                if let a::TypeRef::Named(rec_name) = inner.as_ref() {
                                    self.records.get(rec_name).and_then(|m| m.get(f).copied())
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        current = field_ty;
                    }
                    a::PathSeg::Index(_) => {
                        // Subscript into a Map: advance `current` to the inner record type.
                        if let Some(a::TypeRef::Map { inner, .. }) = current {
                            current = Some(inner.as_ref());
                        }
                    }
                }
            }
            return current.map(|t| self.type_ref_kind(t)).unwrap_or(Kind::Nat);
        }
        // Bare ident — try handler params first.
        if p.segments.is_empty() {
            if let Some((_, ty)) = self.params.iter().find(|(n, _)| n == &p.root) {
                return self.type_ref_kind(ty);
            }
        }
        Kind::Nat
    }

    /// Resolve the SOURCE type name of a path expression — e.g.,
    /// `state.accounts[i]` → `"Account"` when `accounts : Map[N] Account`.
    /// Returns None when the path terminates on a primitive/Bool/unknown type
    /// or doesn't refer into the state.
    fn path_type_name(&self, p: &a::Path) -> Option<String> {
        if p.root != "state" {
            if p.segments.is_empty() {
                if let Some((_, a::TypeRef::Named(n))) =
                    self.params.iter().find(|(n, _)| n == &p.root)
                {
                    return Some(n.clone());
                }
            }
            return None;
        }
        let mut current: Option<&a::TypeRef> = None;
        for seg in &p.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    current = match current {
                        None => self.state_fields.get(f).copied(),
                        Some(a::TypeRef::Named(rec)) => {
                            self.records.get(rec).and_then(|m| m.get(f).copied())
                        }
                        Some(a::TypeRef::Map { inner, .. }) => {
                            if let a::TypeRef::Named(rec) = inner.as_ref() {
                                self.records.get(rec).and_then(|m| m.get(f).copied())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                }
                a::PathSeg::Index(_) => {
                    if let Some(a::TypeRef::Map { inner, .. }) = current {
                        current = Some(inner.as_ref());
                    }
                }
            }
        }
        match current? {
            a::TypeRef::Named(n) => Some(n.clone()),
            _ => None,
        }
    }

    /// Infer the kind of an Expr.
    fn infer(&self, e: &Expr) -> Kind {
        match e {
            Expr::Int(_) => Kind::Nat, // Lean elaborates literals against context.
            Expr::Bool(_) => Kind::Bool,
            Expr::Path(p) => self.path_kind(p),
            Expr::Old(inner) => self.infer(&inner.node),
            Expr::Sum { body, .. } => self.infer(&body.node),
            Expr::Quant { .. } => Kind::Bool,
            Expr::BoolOp { .. } => Kind::Bool,
            Expr::Not(_) => Kind::Bool,
            Expr::Cmp { .. } => Kind::Bool,
            Expr::Arith { lhs, rhs, .. } => {
                let lk = self.infer(&lhs.node);
                let rk = self.infer(&rhs.node);
                // Int dominates Nat; anything with Other stays Nat (safe default).
                match (lk, rk) {
                    (Kind::Int, _) | (_, Kind::Int) => Kind::Int,
                    _ => Kind::Nat,
                }
            }
            Expr::Paren(inner) => self.infer(&inner.node),
            // mul_div_floor/ceil follow the operand types: Int if any of a or
            // b is Int, else Nat. Divisor kind doesn't promote — it's a scale.
            Expr::MulDivFloor { a, b, .. } | Expr::MulDivCeil { a, b, .. } => {
                let ak = self.infer(&a.node);
                let bk = self.infer(&b.node);
                match (ak, bk) {
                    (Kind::Int, _) | (_, Kind::Int) => Kind::Int,
                    _ => Kind::Nat,
                }
            }
            // Match result type: use the first arm's body. Arms must agree;
            // in phase 1 we don't cross-check.
            Expr::Match { arms, .. } => arms
                .first()
                .map(|a| self.infer(&a.body.node))
                .unwrap_or(Kind::Other),
            // Constructor value — sum-type result. Kind is Other because
            // downstream consumers (Map updates, effect assignments) don't
            // need arithmetic promotion for the outer value.
            Expr::Ctor { .. } => Kind::Other,
            // Anonymous record literal — Other (no arithmetic promotion).
            Expr::RecordLit(_) => Kind::Other,
            // Record update produces the same kind as the base.
            Expr::RecordUpdate { base, .. } => self.infer(&base.node),
            // Constructor test → Bool (propositional).
            Expr::IsVariant { .. } => Kind::Bool,
            // Function application — abstract, treat as Other (no promotion).
            Expr::App { .. } => Kind::Other,
            // Postfix field access — abstract, treat as Other.
            Expr::Field { .. } => Kind::Other,
            // `let x = v in body` — kind follows the body (the let is
            // transparent from the caller's perspective).
            Expr::Let { body, .. } => self.infer(&body.node),
        }
    }
}

/// Render typed expression to a Lean-compatible string (unicode operators).
/// Threads a `TypeEnv` through so arithmetic/comparison can promote Nat→Int
/// when operands' kinds differ.
fn expr_to_lean(e: &Expr, ctx: Ctx, consts: ConstTable, env: &TypeEnv) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        // Bool literal in Lean 4 is lowercase `true`/`false` (the `Bool`
        // inductive). `True`/`False` are *Props*, so an effect RHS like
        // `flag := True` would type-error when `flag : Bool`. This was
        // the latent half of issue #8 finding #6 (the cover-witness
        // side used `"0"` for Bool; this side used Prop).
        Expr::Bool(b) => b.to_string(),
        Expr::Path(p) => path_to_lean(p, ctx, /*inside_old=*/ false, consts),
        Expr::Old(inner) => path_or_expr_to_lean_old(&inner.node, ctx, consts, env),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => format!(
            "(\u{2211} {} : {}, {})",
            binder,
            binder_ty,
            expr_to_lean(&body.node, ctx, consts, env)
        ),
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => {
            let sym = match kind {
                a::Quantifier::Forall => "\u{2200}",
                a::Quantifier::Exists => "\u{2203}",
            };
            let lean_ty = match binder_ty.as_str() {
                "U64" | "U32" | "U16" | "U8" | "U128" => "Nat",
                "I64" | "I32" | "I16" | "I8" | "I128" => "Int",
                other => other,
            };
            format!(
                "{} {} : {}, {}",
                sym,
                binder,
                lean_ty,
                expr_to_lean(&body.node, ctx, consts, env)
            )
        }
        Expr::BoolOp { op, lhs, rhs } => {
            let sym = match op {
                a::BoolOp::And => " \u{2227} ",
                a::BoolOp::Or => " \u{2228} ",
                a::BoolOp::Implies => " \u{2192} ",
            };
            format!(
                "{}{}{}",
                expr_to_lean(&lhs.node, ctx, consts, env),
                sym,
                expr_to_lean(&rhs.node, ctx, consts, env)
            )
        }
        Expr::Not(inner) => {
            format!("\u{00AC}({})", expr_to_lean(&inner.node, ctx, consts, env))
        }
        Expr::Cmp { op, lhs, rhs } => {
            let sym = match op {
                a::CmpOp::Eq => "=",
                a::CmpOp::Ne => "\u{2260}",
                a::CmpOp::Le => "\u{2264}",
                a::CmpOp::Ge => "\u{2265}",
                a::CmpOp::Lt => "<",
                a::CmpOp::Gt => ">",
            };
            let (l_str, r_str) =
                render_binary_with_coercion(&lhs.node, &rhs.node, ctx, consts, env);
            format!("{} {} {}", l_str, sym, r_str)
        }
        Expr::Arith { op, lhs, rhs } => {
            let sym = match op {
                a::ArithOp::Add => " + ",
                a::ArithOp::Sub => " - ",
                a::ArithOp::Mul => " * ",
                a::ArithOp::Div => " / ",
                a::ArithOp::Mod => " % ",
            };
            let (l_str, r_str) =
                render_binary_with_coercion(&lhs.node, &rhs.node, ctx, consts, env);
            format!("{}{}{}", l_str, sym, r_str)
        }
        Expr::Paren(inner) => format!("({})", expr_to_lean(&inner.node, ctx, consts, env)),
        Expr::MulDivFloor { a, b, d } => {
            // Lean Int is unbounded — the math simplifies to `(a * b) / d`
            // with integer division. If any operand is Int, the whole expr
            // is Int; otherwise we stay in Nat. Overflow is a Rust-codegen
            // concern, not a proof concern.
            let (a_str, b_str) = render_binary_with_coercion(&a.node, &b.node, ctx, consts, env);
            let d_str = expr_to_lean(&d.node, ctx, consts, env);
            format!("((({}) * ({})) / ({}))", a_str, b_str, d_str)
        }
        Expr::Match { scrutinee, arms } => {
            // Render as Lean's `match ... with | Ctor binder? => body | ...`.
            // If the body doesn't reference the binder, emit `_` instead —
            // Lean's Decidable-synthesis is tripped up by named binders in
            // Prop-valued arms that don't use them.
            let sc = expr_to_lean(&scrutinee.node, ctx, consts, env);
            let mut out = String::new();
            out.push_str("(match ");
            out.push_str(&sc);
            out.push_str(" with");
            for arm in arms {
                let body_str = expr_to_lean(&arm.body.node, ctx, consts, env);
                let binder_used = arm
                    .binder
                    .as_deref()
                    .map(|b| body_mentions_binder(&body_str, b))
                    .unwrap_or(false);
                out.push_str(&format!("\n    | .{}", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push(' ');
                    if binder_used {
                        out.push_str(b);
                    } else {
                        out.push('_');
                    }
                }
                out.push_str(" => ");
                out.push_str(&body_str);
            }
            out.push(')');
            out
        }
        Expr::Ctor { variant, payload } => {
            // Lean anonymous constructor: `.Variant` or `.Variant <payload>`.
            // Payload is typically a record literal or record update; renders
            // verbatim. Lean's elaborator resolves the expected type.
            match payload {
                None => format!(".{}", variant),
                Some(p) => format!(".{} {}", variant, expr_to_lean(&p.node, ctx, consts, env)),
            }
        }
        Expr::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{} := {}", n, expr_to_lean(&v.node, ctx, consts, env)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {} }}", body)
        }
        Expr::RecordUpdate { base, updates } => {
            let base_str = expr_to_lean(&base.node, ctx, consts, env);
            let body = updates
                .iter()
                .map(|(n, v)| format!("{} := {}", n, expr_to_lean(&v.node, ctx, consts, env)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {} with {} }}", base_str, body)
        }
        Expr::IsVariant { scrutinee, variant } => {
            // Route through the per-variant helper when we can resolve the
            // scrutinee's type. `TypeName.isVariant x = true` is always
            // Decidable (Bool equality), unlike a raw match on a Prop.
            // Fallback path (unknown type): inline match, may not elaborate
            // if Lean can't synthesize Decidable.
            let sc = expr_to_lean(&scrutinee.node, ctx, consts, env);
            if let Expr::Path(p) = &scrutinee.node {
                if let Some(ty_name) = env.path_type_name(p) {
                    return format!("({}.is{} {} = true)", ty_name, variant, sc);
                }
            }
            format!("(match {} with | .{} _ => True | _ => False)", sc, variant)
        }
        Expr::MulDivCeil { a, b, d } => {
            // ceil(a*b/d) = (a*b + d - 1) / d   for positive d.
            // Lean: we emit the identity directly. Signed operands still
            // work because Lean's integer division rounds toward zero; for
            // positive `d` and nonnegative `a*b` this matches ceiling.
            // Spec authors assume `d > 0`; downstream proofs rely on that.
            let (a_str, b_str) = render_binary_with_coercion(&a.node, &b.node, ctx, consts, env);
            let d_str = expr_to_lean(&d.node, ctx, consts, env);
            format!(
                "((({}) * ({}) + ({}) - 1) / ({}))",
                a_str, b_str, d_str, d_str
            )
        }
        Expr::App { func, args } => {
            // Lean function application: `f a b c` (space-separated, parenthesized
            // args). Leaves `func` as the raw name — downstream users declare
            // these as uninterpreted helpers (axioms or defs) in a support module.
            let args_str: Vec<String> = args
                .iter()
                .map(|n| format!("({})", expr_to_lean(&n.node, ctx, consts, env)))
                .collect();
            format!("({} {})", func, args_str.join(" "))
        }
        Expr::Field { base, field } => {
            let base_str = expr_to_lean(&base.node, ctx, consts, env);
            format!("({}).{}", base_str, field)
        }
        Expr::Let { name, value, body } => {
            // Lean's `let x := v; body` is semicolon-separated inside a
            // tactic-free term position, which is what ensures/requires give us.
            format!(
                "(let {} := {}; {})",
                name,
                expr_to_lean(&value.node, ctx, consts, env),
                expr_to_lean(&body.node, ctx, consts, env)
            )
        }
    }
}

/// Render both sides of a binary op, inserting a `((x : Int))` coercion on
/// whichever side is Nat when the other is Int. Leaves operand pairs of
/// matching kind untouched.
fn render_binary_with_coercion(
    lhs: &Expr,
    rhs: &Expr,
    ctx: Ctx,
    consts: ConstTable,
    env: &TypeEnv,
) -> (String, String) {
    let lk = env.infer(lhs);
    let rk = env.infer(rhs);
    let l_str = expr_to_lean(lhs, ctx, consts, env);
    let r_str = expr_to_lean(rhs, ctx, consts, env);
    match (lk, rk) {
        (Kind::Nat, Kind::Int) => (format!("((({}) : Int))", l_str), r_str),
        (Kind::Int, Kind::Nat) => (l_str, format!("((({}) : Int))", r_str)),
        _ => (l_str, r_str),
    }
}

/// Render path to Lean form, honoring `state.X` prefix. Bare idents matching
/// a declared constant are substituted with the literal value (pest parity).
fn path_to_lean(p: &a::Path, ctx: Ctx, inside_old: bool, consts: ConstTable) -> String {
    let mut out = String::new();
    let is_state_path = p.root == "state";
    if is_state_path {
        let prefix = if inside_old {
            "s."
        } else {
            match ctx {
                Ctx::Guard => "s.",
                Ctx::Ensures => "s'.",
            }
        };
        out.push_str(prefix);
        for seg in &p.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    if out.ends_with('.') {
                        out.push_str(f);
                    } else {
                        out.push('.');
                        out.push_str(f);
                    }
                }
                a::PathSeg::Index(i) => {
                    out.push('[');
                    out.push_str(i);
                    out.push(']');
                }
            }
        }
        if out.ends_with('.') {
            out.pop();
        }
    } else if p.segments.is_empty() {
        // Bare ident — substitute if declared as a const.
        if let Some(v) = consts.get(&p.root) {
            out.push_str(v);
        } else {
            out.push_str(&p.root);
        }
    } else {
        out.push_str(&p.root);
        for seg in &p.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    out.push('.');
                    out.push_str(f);
                }
                a::PathSeg::Index(i) => {
                    out.push('[');
                    out.push_str(i);
                    out.push(']');
                }
            }
        }
    }
    out
}

fn path_or_expr_to_lean_old(inner: &Expr, ctx: Ctx, consts: ConstTable, env: &TypeEnv) -> String {
    match inner {
        Expr::Path(p) => path_to_lean(p, ctx, /*inside_old=*/ true, consts),
        other => match ctx {
            Ctx::Guard => {
                let rendered = expr_to_lean(other, Ctx::Guard, consts, env);
                format!("\u{00AB}old({})\u{00BB}", strip_state_prefix(&rendered))
            }
            Ctx::Ensures => expr_to_lean(other, Ctx::Guard, consts, env),
        },
    }
}

/// Check if an arm body string mentions an identifier as a whole word.
/// Used to decide whether to preserve `binder` or emit `_` in match arms.
fn body_mentions_binder(body: &str, binder: &str) -> bool {
    if binder.is_empty() {
        return false;
    }
    let bytes = body.as_bytes();
    let target = binder.as_bytes();
    let n = bytes.len();
    let m = target.len();
    if m > n {
        return false;
    }
    let is_ident_char = |c: u8| (c as char).is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i + m <= n {
        if &bytes[i..i + m] == target {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + m == n || !is_ident_char(bytes[i + m]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn strip_state_prefix(s: &str) -> String {
    s.strip_prefix("s.")
        .or_else(|| s.strip_prefix("s'."))
        .map(|r| r.to_string())
        .unwrap_or_else(|| s.to_string())
}

/// Render typed expression to a Rust-compatible string (ASCII operators).
fn expr_to_rust(e: &Expr, ctx: Ctx, consts: ConstTable) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Path(p) => path_to_rust(p, ctx, false, consts),
        Expr::Old(inner) => match &inner.node {
            Expr::Path(p) => path_to_rust(p, ctx, true, consts),
            other => format!("/*old({})*/", expr_to_rust(other, ctx, consts)),
        },
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => format!(
            "sum_over::<{}>(|{}| {})",
            binder_ty,
            binder,
            expr_to_rust(&body.node, ctx, consts)
        ),
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body: _,
        } => {
            // Quantifiers don't lower to a property-function body directly: the
            // universal case needs harness-level `kani::any()` scaffolding that's
            // emitted by the backend, not inlined here. Surface the sentinel so
            // the caller can replace the generated function body with a skip
            // marker. See `rust_expr_is_unsupported` in check.rs.
            let kind_name = match kind {
                a::Quantifier::Forall => "forall",
                a::Quantifier::Exists => "exists",
            };
            format!(
                "/* QEDGEN_UNSUPPORTED_QUANTIFIER: {} {} : {} — lower at harness level */",
                kind_name, binder, binder_ty
            )
        }
        Expr::BoolOp { op, lhs, rhs } => {
            let lhs_r = expr_to_rust(&lhs.node, ctx, consts);
            let rhs_r = expr_to_rust(&rhs.node, ctx, consts);
            match op {
                a::BoolOp::And => format!("({}) && ({})", lhs_r, rhs_r),
                a::BoolOp::Or => format!("({}) || ({})", lhs_r, rhs_r),
                // `a implies b` ≡ `!a || b`; parenthesize both sides to survive
                // surrounding precedence (matters once callers compose via `&&`/`||`).
                a::BoolOp::Implies => format!("(!({})) || ({})", lhs_r, rhs_r),
            }
        }
        Expr::Not(inner) => format!("!({})", expr_to_rust(&inner.node, ctx, consts)),
        Expr::Cmp { op, lhs, rhs } => {
            let sym = match op {
                a::CmpOp::Eq => "==",
                a::CmpOp::Ne => "!=",
                a::CmpOp::Le => "<=",
                a::CmpOp::Ge => ">=",
                a::CmpOp::Lt => "<",
                a::CmpOp::Gt => ">",
            };
            format!(
                "{} {} {}",
                expr_to_rust(&lhs.node, ctx, consts),
                sym,
                expr_to_rust(&rhs.node, ctx, consts)
            )
        }
        Expr::Arith { op, lhs, rhs } => {
            let sym = match op {
                a::ArithOp::Add => " + ",
                a::ArithOp::Sub => " - ",
                a::ArithOp::Mul => " * ",
                a::ArithOp::Div => " / ",
                a::ArithOp::Mod => " % ",
            };
            format!(
                "{}{}{}",
                expr_to_rust(&lhs.node, ctx, consts),
                sym,
                expr_to_rust(&rhs.node, ctx, consts)
            )
        }
        Expr::Paren(inner) => format!("({})", expr_to_rust(&inner.node, ctx, consts)),
        Expr::MulDivFloor { a, b, d } => format!(
            "mul_div_floor_u128({}, {}, {})",
            expr_to_rust(&a.node, ctx, consts),
            expr_to_rust(&b.node, ctx, consts),
            expr_to_rust(&d.node, ctx, consts)
        ),
        Expr::MulDivCeil { a, b, d } => format!(
            "mul_div_ceil_u128({}, {}, {})",
            expr_to_rust(&a.node, ctx, consts),
            expr_to_rust(&b.node, ctx, consts),
            expr_to_rust(&d.node, ctx, consts)
        ),
        Expr::Match { scrutinee, arms } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts);
            let mut out = format!("match {} {{", sc);
            for arm in arms {
                out.push_str(&format!("\n    {}::{}", "/* ty */", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push_str(&format!("({})", b));
                }
                out.push_str(" => ");
                out.push_str(&expr_to_rust(&arm.body.node, ctx, consts));
                out.push(',');
            }
            out.push_str("\n}");
            out
        }
        Expr::Ctor { variant, payload } => match payload {
            None => format!("{}::{}", "/* ty */", variant),
            Some(p) => format!(
                "{}::{}({})",
                "/* ty */",
                variant,
                expr_to_rust(&p.node, ctx, consts)
            ),
        },
        Expr::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {} }}", "/* ty */", body)
        }
        Expr::RecordUpdate { base, updates } => {
            let base_str = expr_to_rust(&base.node, ctx, consts);
            let body = updates
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {}, ..{} }}", "/* ty */", body, base_str)
        }
        Expr::IsVariant { scrutinee, variant } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts);
            format!("matches!({}, {}::{}(..))", sc, "/* ty */", variant)
        }
        Expr::App { func, args } => {
            let args_str: Vec<String> = args
                .iter()
                .map(|n| expr_to_rust(&n.node, ctx, consts))
                .collect();
            format!("{}({})", func, args_str.join(", "))
        }
        Expr::Field { base, field } => {
            let base_str = expr_to_rust(&base.node, ctx, consts);
            format!("{}.{}", base_str, field)
        }
        Expr::Let { name, value, body } => {
            // Rust lowers a let-in expression to a block. Parentheses are
            // safe around the block for embedding in larger expressions.
            format!(
                "({{ let {} = {}; {} }})",
                name,
                expr_to_rust(&value.node, ctx, consts),
                expr_to_rust(&body.node, ctx, consts)
            )
        }
    }
}

fn path_to_rust(p: &a::Path, _ctx: Ctx, _inside_old: bool, consts: ConstTable) -> String {
    let mut out = String::new();
    if p.segments.is_empty() && p.root != "state" {
        // Bare ident — substitute if declared as a const (pest parity).
        if let Some(v) = consts.get(&p.root) {
            return v.clone();
        }
    }
    // B12 (v2.6.1): `state.X` lowers to `s.X` — every Rust consumer (property
    // fn bodies, transition-fn assume predicates, abort.rust_expr, etc.) binds
    // state to a parameter named `s`. Previously we emitted `state` as-is and
    // relied on a post-hoc `translate_guard_to_rust` string replace to fix it,
    // which covered `requires` but missed property bodies consumed raw via
    // `prop.rust_expression`.
    if p.root == "state" {
        out.push('s');
    } else {
        out.push_str(&p.root);
    }
    for seg in &p.segments {
        match seg {
            a::PathSeg::Field(f) => {
                out.push('.');
                out.push_str(f);
            }
            a::PathSeg::Index(i) => {
                out.push('[');
                out.push_str(i);
                out.push(']');
            }
        }
    }
    out
}

// ============================================================================
// Type reference rendering (to the legacy type-string form)
// ============================================================================

/// True if `name` is used as the inner value type of any `Map[N] T` field
/// in any record or state ADT variant anywhere in `spec`. Sum types that
/// qualify get inductive Lean codegen; other ADTs stay on the flatten path.
fn is_map_value_sum_type(name: &str, spec: &a::Spec) -> bool {
    // Check all record fields and ADT variant fields for `Map[N] <name>`.
    fn type_ref_uses_map_value(t: &a::TypeRef, name: &str) -> bool {
        match t {
            a::TypeRef::Map { inner, .. } => match inner.as_ref() {
                a::TypeRef::Named(n) => n == name,
                _ => false,
            },
            _ => false,
        }
    }
    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Record(r) => {
                for f in &r.fields {
                    if type_ref_uses_map_value(&f.ty, name) {
                        return true;
                    }
                }
            }
            TopItem::Adt(adt) => {
                for v in &adt.variants {
                    for f in &v.fields {
                        if type_ref_uses_map_value(&f.ty, name) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn type_ref_to_string(t: &a::TypeRef) -> String {
    match t {
        a::TypeRef::Named(n) => n.clone(),
        a::TypeRef::Param(head, tail) => format!("{} {}", head, tail),
        a::TypeRef::Map { bound, inner } => {
            format!("Map[{}] {}", bound, type_ref_to_string(inner))
        }
        a::TypeRef::Fin { bound } => format!("Fin[{}]", bound),
    }
}

/// Resolve a type reference through a table of aliases until we hit a
/// non-alias target. Cyclic aliases bottom out on a fixed number of hops.
///
/// Scaffolding: used once the adapter grows alias-aware coercion for guard
/// expressions (e.g., `type Amount = U128` appearing in a requires/effect).
/// Kept near the typed-AST surface so it's ready when that pass lands.
#[allow(dead_code)]
fn resolve_type_alias<'a>(
    name: &str,
    aliases: &'a std::collections::BTreeMap<String, a::TypeRef>,
) -> Option<&'a a::TypeRef> {
    let mut current = aliases.get(name)?;
    for _ in 0..16 {
        if let a::TypeRef::Named(n) = current {
            if let Some(next) = aliases.get(n) {
                current = next;
                continue;
            }
        }
        return Some(current);
    }
    Some(current)
}

// ============================================================================
// Effect rendering: (field_name, op, value_string)
// ============================================================================

fn render_effect(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
) -> (String, String, String) {
    // Field name: preserve subscript syntax as-is (e.g., `accounts[i].capital`).
    let field = {
        let mut s = stmt.lhs.root.clone();
        for seg in &stmt.lhs.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    s.push('.');
                    s.push_str(f);
                }
                a::PathSeg::Index(i) => {
                    s.push('[');
                    s.push_str(i);
                    s.push(']');
                }
            }
        }
        s
    };
    // Per-effect semantic tag (v2.7 G3):
    //   - "add" / "sub"               = checked (default)
    //   - "add_sat" / "sub_sat"       = saturating (`+=!` / `-=!`)
    //   - "add_wrap" / "sub_wrap"     = wrapping   (`+=?` / `-=?`)
    // Existing code paths that test `kind == "add"` continue to work for the
    // default case (the one they were written against). Codegen branches on
    // the full tag when the distinction matters.
    let op = match stmt.op {
        a::EffectOp::Add => "add",
        a::EffectOp::AddSat => "add_sat",
        a::EffectOp::AddWrap => "add_wrap",
        a::EffectOp::Sub => "sub",
        a::EffectOp::SubSat => "sub_sat",
        a::EffectOp::SubWrap => "sub_wrap",
        a::EffectOp::Set => "set",
    };
    // Value string — match pest's effect_value_to_string which strips
    // `state.` prefix for qualified refs and leaves bare idents / integers.
    let value = match &stmt.rhs.node {
        Expr::Int(v) => v.to_string(),
        Expr::Path(p) => {
            let is_param = p.segments.is_empty() && params.iter().any(|(n, _)| n == &p.root);
            if is_param {
                p.root.clone()
            } else if p.root == "state" {
                // state.X → X (strip prefix, matches pest output)
                let mut s = String::new();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            if !s.is_empty() {
                                s.push('.');
                            }
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            } else {
                // Bare path that isn't a param — emit as-is
                let mut s = p.root.clone();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            s.push('.');
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            }
        }
        // Complex RHS (match / ctor / record update / arithmetic):
        // render in Lean form. The effect value is consumed by lean_gen,
        // so Lean-form rendering is what matters. Build a minimal type env
        // for coercion — params only; spec-wide types would require the
        // full env but aren't usually relevant on effect RHS.
        other => {
            let env = TypeEnv::default().with_params(&[]);
            let params_slice: Vec<(String, a::TypeRef)> = params
                .iter()
                .map(|(n, t)| (n.clone(), string_to_typeref_best_effort(t)))
                .collect();
            let _ = params_slice; // future: plumb real params here for coercion
            expr_to_lean(other, Ctx::Guard, consts, &env)
        }
    };
    (field, op.to_string(), value)
}

/// Best-effort reconstruction of a `TypeRef` from its rendered string form,
/// used only inside `render_effect` where we don't have the original AST.
fn string_to_typeref_best_effort(s: &str) -> a::TypeRef {
    a::TypeRef::Named(s.trim().to_string())
}

// ============================================================================
// sBPF instruction adapter
// ============================================================================

/// Render a simple guard expression into the space-separated ASCII triple
/// form consumed by `derive_guard_hypotheses` in `lean_gen`:
///   `field == RHS`, `field >= RHS`, etc.
/// When `resolve_consts` is true, bare identifiers that are declared constants
/// are substituted with their values (for the `checks` form). Otherwise names
/// are preserved verbatim (for the `checks_raw` form).
fn render_sbpf_check(e: &Expr, consts: ConstTable, resolve_consts: bool) -> String {
    fn render(e: &Expr, consts: ConstTable, resolve_consts: bool) -> String {
        match e {
            Expr::Int(v) => v.to_string(),
            Expr::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Expr::Path(p) => {
                // Render as root[.seg]* with no state prefix substitution.
                if p.segments.is_empty() {
                    if resolve_consts {
                        if let Some(v) = consts.get(&p.root) {
                            return v.clone();
                        }
                    }
                    return p.root.clone();
                }
                let mut s = p.root.clone();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            s.push('.');
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            }
            Expr::Paren(inner) => render(&inner.node, consts, resolve_consts),
            Expr::Cmp { op, lhs, rhs } => {
                let sym = match op {
                    a::CmpOp::Eq => "==",
                    a::CmpOp::Ne => "!=",
                    a::CmpOp::Le => "<=",
                    a::CmpOp::Ge => ">=",
                    a::CmpOp::Lt => "<",
                    a::CmpOp::Gt => ">",
                };
                format!(
                    "{} {} {}",
                    render(&lhs.node, consts, resolve_consts),
                    sym,
                    render(&rhs.node, consts, resolve_consts)
                )
            }
            Expr::Arith { op, lhs, rhs } => {
                let sym = match op {
                    a::ArithOp::Add => "+",
                    a::ArithOp::Sub => "-",
                    a::ArithOp::Mul => "*",
                    a::ArithOp::Div => "/",
                    a::ArithOp::Mod => "%",
                };
                format!(
                    "{} {} {}",
                    render(&lhs.node, consts, resolve_consts),
                    sym,
                    render(&rhs.node, consts, resolve_consts)
                )
            }
            // Fallback for unexpected shapes — pretty-print a minimal Lean-ish form.
            other => {
                let env = TypeEnv::default();
                expr_to_lean(other, Ctx::Guard, consts, &env)
            }
        }
    }
    render(e, consts, resolve_consts)
}

/// Translate an `InstructionDecl` into the legacy `ParsedInstruction` shape.
fn adapt_instruction(instr: &a::InstructionDecl, top_consts: ConstTable) -> ParsedInstruction {
    let mut discriminant: Option<String> = None;
    let mut entry: Option<u64> = None;
    let mut constants: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<ParsedErrorCode> = Vec::new();
    let mut input_layout: Vec<ParsedLayoutField> = Vec::new();
    let mut insn_layout: Vec<ParsedLayoutField> = Vec::new();
    let mut guard_decls: Vec<&a::GuardDecl> = Vec::new();
    let mut prop_decls: Vec<&a::SbpfPropertyDecl> = Vec::new();

    for item in &instr.items {
        match item {
            a::InstructionItem::Discriminant(d) => discriminant = Some(d.clone()),
            a::InstructionItem::Entry(n) => entry = Some(*n),
            a::InstructionItem::Const { name, value } => {
                constants.push((name.clone(), value.to_string()));
            }
            a::InstructionItem::Errors(entries) => {
                for e in entries {
                    errors.push(ParsedErrorCode {
                        name: e.name.clone(),
                        value: e.code,
                        description: e.description.clone(),
                    });
                }
            }
            a::InstructionItem::InputLayout(fs) => {
                for f in fs {
                    input_layout.push(ParsedLayoutField {
                        name: f.name.clone(),
                        field_type: f.field_type.clone(),
                        offset: f.offset,
                        description: f.description.clone(),
                    });
                }
            }
            a::InstructionItem::InsnLayout(fs) => {
                for f in fs {
                    insn_layout.push(ParsedLayoutField {
                        name: f.name.clone(),
                        field_type: f.field_type.clone(),
                        offset: f.offset,
                        description: f.description.clone(),
                    });
                }
            }
            a::InstructionItem::Guard(g) => guard_decls.push(g),
            a::InstructionItem::SbpfProperty(p) => prop_decls.push(p),
        }
    }

    // Build a merged const table: top-level constants + this instruction's
    // local constants. Instruction-local wins on conflict (pest parity).
    let mut merged = top_consts.clone();
    for (name, value) in &constants {
        merged.insert(name.clone(), value.clone());
    }
    let merged_consts: ConstTable = &merged;

    let guards: Vec<ParsedGuard> = guard_decls
        .iter()
        .map(|g| {
            let (checks, checks_raw) = match &g.checks {
                Some(e) => (
                    Some(render_sbpf_check(&e.node, merged_consts, true)),
                    Some(render_sbpf_check(&e.node, merged_consts, false)),
                ),
                None => (None, None),
            };
            ParsedGuard {
                name: g.name.clone(),
                doc: g.doc.clone(),
                checks,
                checks_raw,
                error: g.error.clone(),
                fuel: g.fuel,
            }
        })
        .collect();

    let properties: Vec<ParsedSbpfProperty> =
        prop_decls.iter().map(|p| adapt_sbpf_property(p)).collect();

    ParsedInstruction {
        name: instr.name.clone(),
        doc: instr.doc.clone(),
        discriminant,
        entry,
        constants,
        errors,
        input_layout,
        insn_layout,
        guards,
        properties,
    }
}

/// Pending CPI envelope data accumulated while scanning an sBPF property's
/// clauses: (program, instruction, fields).
type PendingCpi = (String, String, Vec<(String, String)>);

fn adapt_sbpf_property(p: &a::SbpfPropertyDecl) -> ParsedSbpfProperty {
    // Decide kind from the clauses. Later clauses override earlier ones when
    // they set the same field. The presence of certain clauses determines the
    // variant.
    let mut scope_targets: Option<Vec<String>> = None;
    let mut flow: Option<(String, FlowKind)> = None;
    let mut cpi: Option<PendingCpi> = None;
    let mut after_all_guards = false;
    let mut exit: Option<u64> = None;
    let mut has_expr = false;

    for clause in &p.clauses {
        match clause {
            a::SbpfPropClause::Expr(_) => has_expr = true,
            a::SbpfPropClause::PreservedBy(_) => {}
            a::SbpfPropClause::Scope(names) => scope_targets = Some(names.clone()),
            a::SbpfPropClause::Flow { target, kind } => {
                let k = match kind {
                    a::SbpfFlowKind::FromSeeds(xs) => FlowKind::FromSeeds(xs.clone()),
                    a::SbpfFlowKind::Through(xs) => FlowKind::Through(xs.clone()),
                };
                flow = Some((target.clone(), k));
            }
            a::SbpfPropClause::Cpi {
                program,
                instruction,
                fields,
            } => {
                cpi = Some((program.clone(), instruction.clone(), fields.clone()));
            }
            a::SbpfPropClause::AfterAllGuards => after_all_guards = true,
            a::SbpfPropClause::Exit(n) => exit = Some(*n),
        }
    }

    let _ = has_expr; // accepted but currently unused for routing
    let kind = if let Some(targets) = scope_targets {
        SbpfPropertyKind::Scope { targets }
    } else if let Some((target, k)) = flow {
        SbpfPropertyKind::Flow { target, kind: k }
    } else if let Some((program, instruction, fields)) = cpi {
        SbpfPropertyKind::Cpi {
            program,
            instruction,
            fields,
        }
    } else if after_all_guards || exit.is_some() {
        SbpfPropertyKind::HappyPath {
            exit_code: exit.map(|n| n.to_string()).unwrap_or_default(),
        }
    } else {
        // Either an explicit `expr` body or empty — the generic stub covers both.
        SbpfPropertyKind::Generic
    };

    ParsedSbpfProperty {
        name: p.name.clone(),
        doc: p.doc.clone(),
        kind,
    }
}

// ============================================================================
// Top-level adapter
// ============================================================================

/// Convenience: parse a spec source string into a `ParsedSpec` in one step.
/// Used by tests and internal code paths that don't have a file on disk.
pub fn parse_str(src: &str) -> anyhow::Result<ParsedSpec> {
    let typed = crate::chumsky_parser::parse(src).map_err(|errs| {
        let msg = errs
            .iter()
            .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, src)))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("parse error:\n{}", msg)
    })?;
    Ok(adapt(&typed))
}

/// Translate the typed AST into a `ParsedSpec` compatible with current consumers.
pub fn adapt(spec: &a::Spec) -> ParsedSpec {
    let mut out = ParsedSpec {
        program_name: spec.name.clone(),
        ..ParsedSpec::default()
    };

    // First pass: collect constants so guard rendering can substitute them.
    let mut consts_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for Node { node, .. } in &spec.items {
        if let TopItem::Const { name, value } = node {
            consts_map.insert(name.clone(), value.to_string());
        }
    }
    let consts: ConstTable = &consts_map;

    // Build the type environment for arithmetic coercion.
    let env = TypeEnv::from_spec(spec);

    let mut constants = Vec::new();

    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Const { name, value } => {
                constants.push((name.clone(), value.to_string()));
            }
            TopItem::Record(r) => {
                out.records.push(ParsedRecordType {
                    name: r.name.clone(),
                    fields: r
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), type_ref_to_string(&f.ty)))
                        .collect(),
                });
            }
            TopItem::Adt(adt) => {
                // Error ADT: populate error_codes / valued_errors.
                if adt.name == "Error" {
                    for v in &adt.variants {
                        out.error_codes.push(v.name.clone());
                        if v.code.is_some() || v.description.is_some() {
                            out.valued_errors.push(ParsedErrorCode {
                                name: v.name.clone(),
                                value: v.code,
                                description: v.description.clone(),
                            });
                        }
                    }
                } else if is_map_value_sum_type(&adt.name, spec) {
                    // Real sum type used as a Map value → emit as proper Lean
                    // `inductive` later; preserve variant structure here.
                    let variants = adt
                        .variants
                        .iter()
                        .map(|v| ParsedVariant {
                            name: v.name.clone(),
                            fields: v
                                .fields
                                .iter()
                                .map(|f| (f.name.clone(), type_ref_to_string(&f.ty)))
                                .collect(),
                        })
                        .collect();
                    out.sum_types.push(ParsedSumType {
                        name: adt.name.clone(),
                        variants,
                    });
                } else {
                    // State-ish ADT: collect lifecycle from variant names,
                    // fields from the payload-carrying variant(s). Flattened
                    // representation matches existing transition codegen.
                    let lifecycle: Vec<String> =
                        adt.variants.iter().map(|v| v.name.clone()).collect();
                    // B1 (v2.6): flatten variant fields into the state-field
                    // list BUT deduplicate by name. Before this, each variant
                    // contributed the full record to `fields`, producing e.g.
                    //     struct State {
                    //         pool: u64, status: u8,
                    //         pool: u64, status: u8,   // duplicate from Frozen
                    //         pool: u64, status: u8,   // duplicate from Settled
                    //     }
                    // in the Kani harness — invalid Rust. First occurrence
                    // wins on name collision (variants usually share the same
                    // field shape). If two variants declare the same field
                    // name with different types, the downstream `check.rs`
                    // lint surfaces the mismatch. Proper enum+match codegen
                    // is tracked separately (release notes).
                    let mut fields: Vec<(String, String)> = Vec::new();
                    let mut seen: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for variant in &adt.variants {
                        for f in &variant.fields {
                            if seen.insert(f.name.clone()) {
                                fields.push((f.name.clone(), type_ref_to_string(&f.ty)));
                            }
                        }
                    }
                    out.account_types.push(ParsedAccountType {
                        name: adt.name.clone(),
                        fields,
                        lifecycle,
                        pda_ref: None,
                    });
                }
            }
            TopItem::Handler(h) => {
                // If the handler has a `match` clause, expand into one
                // synthetic handler per arm. Otherwise, single handler.
                let expanded = expand_handler(h, consts, &env);
                out.handlers.extend(expanded);
            }
            TopItem::Property(p) => {
                let lean = expr_to_lean(&p.body.node, Ctx::Guard, consts, &env);
                let rust = expr_to_rust(&p.body.node, Ctx::Guard, consts);
                let preserved = match &p.preserved_by {
                    // `preserved_by all` — kept as the sentinel "all".
                    // Expanded to the full handler-name list below after all
                    // handlers are known (matches pest parity).
                    a::PreservedBy::All => vec!["all".to_string()],
                    a::PreservedBy::Some(xs) => xs.clone(),
                };
                out.properties.push(ParsedProperty {
                    name: p.name.clone(),
                    expression: Some(lean),
                    rust_expression: Some(rust),
                    preserved_by: preserved,
                });
            }
            TopItem::Cover(c) => {
                out.covers.push(ParsedCover {
                    name: c.name.clone(),
                    traces: c.traces.clone(),
                    reachable: c
                        .reachable
                        .iter()
                        .map(|(op, when)| {
                            (
                                op.clone(),
                                when.as_ref()
                                    .map(|e| expr_to_lean(&e.node, Ctx::Guard, consts, &env)),
                            )
                        })
                        .collect(),
                });
            }
            TopItem::Liveness(l) => {
                // Strip the type prefix: `State.Active` → `Active`.
                // Legacy code consumes the bare variant name.
                let last = |q: &crate::ast::QualifiedPath| -> String {
                    q.0.last().cloned().unwrap_or_default()
                };
                out.liveness_props.push(ParsedLiveness {
                    name: l.name.clone(),
                    from_state: last(&l.from_state),
                    leads_to_state: last(&l.to_state),
                    via_ops: l.via.clone(),
                    within_steps: Some(l.within),
                });
            }
            TopItem::Invariant(i) => {
                let desc = match &i.body {
                    a::InvariantBody::Expr(e) => expr_to_lean(&e.node, Ctx::Guard, consts, &env),
                    a::InvariantBody::Description(s) => s.clone(),
                };
                out.invariants.push((i.name.clone(), desc));
            }
            TopItem::Pda(p) => {
                let seeds: Vec<String> = p
                    .seeds
                    .iter()
                    .map(|s| match s {
                        a::PdaSeed::Literal(lit) => format!("\"{}\"", lit),
                        a::PdaSeed::Ident(id) => id.clone(),
                    })
                    .collect();
                out.pdas.push(ParsedPda {
                    name: p.name.clone(),
                    seeds,
                });
            }
            TopItem::Event(ev) => {
                out.events.push(ParsedEvent {
                    name: ev.name.clone(),
                    fields: ev
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), type_ref_to_string(&f.ty)))
                        .collect(),
                });
            }
            TopItem::TypeAlias(ta) => {
                out.type_aliases
                    .push((ta.name.clone(), type_ref_to_string(&ta.target)));
            }
            TopItem::ProgramId(pid) => {
                out.program_id = Some(pid.clone());
            }
            TopItem::Pubkey(p) => {
                out.pubkeys.push(ParsedPubkey {
                    name: p.name.clone(),
                    chunks: p.chunks.iter().map(|c| c.to_string()).collect(),
                });
            }
            TopItem::Errors(entries) => {
                // Mirror ADT-Error behavior: populate error_codes and valued_errors.
                for e in entries {
                    out.error_codes.push(e.name.clone());
                    if e.code.is_some() || e.description.is_some() {
                        out.valued_errors.push(ParsedErrorCode {
                            name: e.name.clone(),
                            value: e.code,
                            description: e.description.clone(),
                        });
                    }
                }
            }
            TopItem::Instruction(instr) => {
                out.instructions.push(adapt_instruction(instr, consts));
            }
            TopItem::Environment(envd) => {
                let mut mutates: Vec<(String, String)> = Vec::new();
                let mut constraints_lean: Vec<String> = Vec::new();
                let mut constraints_rust: Vec<String> = Vec::new();
                for Node { node: c, .. } in &envd.clauses {
                    match c {
                        a::EnvClause::Mutates { field, ty } => {
                            mutates.push((field.clone(), ty.clone()));
                        }
                        a::EnvClause::Constraint(e) => {
                            constraints_lean.push(expr_to_lean(
                                &e.node,
                                Ctx::Ensures,
                                consts,
                                &env,
                            ));
                            constraints_rust.push(expr_to_rust(&e.node, Ctx::Ensures, consts));
                        }
                    }
                }
                out.environments.push(ParsedEnvironment {
                    name: envd.name.clone(),
                    mutates,
                    constraints: constraints_lean,
                    constraints_rust,
                });
            }
            TopItem::Interface(iface) => {
                out.interfaces.push(adapt_interface(iface, consts, &env));
            }
            TopItem::Pragma(p) => {
                // Record the pragma name for target inference. Any given
                // pragma may appear at most once per spec; duplicates are
                // flagged at lint time, not here.
                out.pragmas.push(p.name.clone());

                // Inline-adapt each nested item. The parser restricts pragma
                // bodies to a whitelist (const/pubkey/assembly/instruction/
                // errors), so only those cases matter.
                for Node { node: inner, .. } in &p.items {
                    match inner {
                        TopItem::Const { name, value } => {
                            constants.push((name.clone(), value.to_string()));
                        }
                        TopItem::Pubkey(pk) => {
                            out.pubkeys.push(ParsedPubkey {
                                name: pk.name.clone(),
                                chunks: pk.chunks.iter().map(|c| c.to_string()).collect(),
                            });
                        }
                        TopItem::Instruction(instr) => {
                            out.instructions.push(adapt_instruction(instr, consts));
                        }
                        TopItem::Errors(entries) => {
                            for e in entries {
                                out.error_codes.push(e.name.clone());
                                if e.code.is_some() || e.description.is_some() {
                                    out.valued_errors.push(ParsedErrorCode {
                                        name: e.name.clone(),
                                        value: e.code,
                                        description: e.description.clone(),
                                    });
                                }
                            }
                        }
                        // Grammar already rejects non-whitelisted items; this
                        // arm is defensive and silently ignores anything that
                        // slipped through (would indicate a grammar bug).
                        _ => {}
                    }
                }
            }
        }
    }

    // Expand `preserved_by all` to the full handler-name list (pest parity).
    let all_handler_names: Vec<String> = out.handlers.iter().map(|h| h.name.clone()).collect();
    for prop in &mut out.properties {
        if prop.preserved_by.len() == 1 && prop.preserved_by[0] == "all" {
            prop.preserved_by = all_handler_names.clone();
        }
    }

    // Link account_types to PDAs by case-insensitive name match (pest parity).
    for acct in &mut out.account_types {
        if acct.pda_ref.is_none() {
            let lower = acct.name.to_lowercase();
            if let Some(pda) = out.pdas.iter().find(|p| p.name.to_lowercase() == lower) {
                acct.pda_ref = Some(pda.name.clone());
            }
        }
    }

    if let Some(first) = out.account_types.first() {
        out.state_fields = first.fields.clone();
        out.lifecycle_states = first.lifecycle.clone();
    }

    out.constants = constants;
    out
}

/// Expand a handler declaration into one or more `ParsedHandler`s.
/// Handlers without a `branch` clause produce exactly one. Handlers with
/// branches produce one synthetic handler per arm, each carrying the
/// parent's auth/accounts/requires plus the arm's guard and body.
fn expand_handler(
    h: &a::HandlerDecl,
    consts: ConstTable,
    base_env: &TypeEnv,
) -> Vec<ParsedHandler> {
    // Per-handler env carries the handler's params for bare-ident lookup.
    let env = TypeEnv {
        state_fields: base_env.state_fields.clone(),
        records: base_env.records.clone(),
        params: h.params.iter().map(|f| (f.name.clone(), &f.ty)).collect(),
    };
    let env = &env;
    // Detect a single branch clause (phase 1: at most one branch per handler).
    let match_clause: Option<&a::MatchClause> = h.clauses.iter().find_map(|c| match &c.node {
        a::HandlerClause::Match(b) => Some(b),
        _ => None,
    });

    let Some(branch) = match_clause else {
        return vec![adapt_handler(h, consts, env)];
    };

    // Build a shared base handler (parent without the branch clause).
    let base = adapt_handler(h, consts, env);

    // Accumulate negated guards so that earlier arms' failure implies
    // later arms' precondition (first-match semantics).
    let mut prior_conds: Vec<(String, String)> = Vec::new(); // (lean, rust) negations
    let mut out = Vec::with_capacity(branch.arms.len());

    for arm in &branch.arms {
        let mut synth = base.clone();
        synth.name = format!("{}_{}", h.name, arm.label);

        // Add all prior-arm negations to this arm's requires.
        for (lean_neg, rust_neg) in &prior_conds {
            synth.requires.push(ParsedRequires {
                lean_expr: lean_neg.clone(),
                rust_expr: rust_neg.clone(),
                error_name: None,
            });
        }

        // Current arm's guard (if any) becomes a requires; negation is
        // recorded for subsequent arms.
        if let Some(guard) = &arm.guard {
            let lean = expr_to_lean(&guard.node, Ctx::Guard, consts, env);
            let rust = expr_to_rust(&guard.node, Ctx::Guard, consts);
            synth.requires.push(ParsedRequires {
                lean_expr: lean.clone(),
                rust_expr: rust.clone(),
                error_name: None,
            });
            prior_conds.push((format!("\u{00AC}({})", lean), format!("!({})", rust)));
        }

        // Arm body: abort → additional aborting requires; effect → effects
        match &arm.body {
            a::MatchBody::Abort(err) => {
                // Aborting case: synth is guaranteed to fail if its arm fires.
                // Express as `requires false else <err>` so the handler aborts
                // when reached. The `false` is written as `0 == 1` for
                // downstream simplicity (no dedicated False literal).
                synth.requires.push(ParsedRequires {
                    lean_expr: "0 = 1".to_string(),
                    rust_expr: "false".to_string(),
                    error_name: Some(err.clone()),
                });
            }
            a::MatchBody::Effect(stmts) => {
                for Node { node: stmt, .. } in stmts {
                    synth
                        .effects
                        .push(render_effect(stmt, &base.takes_params, consts));
                }
            }
            a::MatchBody::Noop => {}
        }

        out.push(synth);
    }

    out
}

fn adapt_handler(h: &a::HandlerDecl, consts: ConstTable, env: &TypeEnv) -> ParsedHandler {
    let params: Vec<(String, String)> = h
        .params
        .iter()
        .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
        .collect();

    // `on_account` is the type prefix of the pre-state ref, if qualified.
    //   `Loan.Active` → on_account = Some("Loan"), pre_status = Some("Active")
    //   `Active`      → on_account = None,         pre_status = Some("Active")
    let on_account = h.pre.as_ref().and_then(|p| {
        if p.0.len() >= 2 {
            p.0.get(p.0.len() - 2).cloned()
        } else {
            None
        }
    });

    let mut handler = ParsedHandler {
        name: h.name.clone(),
        doc: h.doc.clone(),
        who: None,
        on_account,
        pre_status: h.pre.as_ref().and_then(|p| p.0.last().cloned()),
        post_status: h.post.as_ref().and_then(|p| p.0.last().cloned()),
        takes_params: params.clone(),
        guard_str: None,
        guard_str_rust: None,
        aborts_if: Vec::new(),
        requires: Vec::new(),
        ensures: Vec::new(),
        modifies: None,
        let_bindings: Vec::new(),
        aborts_total: false,
        permissionless: false,
        effects: Vec::new(),
        accounts: Vec::new(),
        transfers: Vec::new(),
        emits: Vec::new(),
        invariants: Vec::new(),
        properties: Vec::new(),
        calls: Vec::new(),
    };

    for Node { node: clause, .. } in &h.clauses {
        match clause {
            a::HandlerClause::Auth(actor) => handler.who = Some(actor.clone()),
            a::HandlerClause::Accounts(descs) => {
                for d in descs {
                    let mut acc = ParsedHandlerAccount {
                        name: d.name.clone(),
                        is_signer: false,
                        is_writable: false,
                        is_program: false,
                        pda_seeds: None,
                        account_type: None,
                        authority: None,
                    };
                    for attr in &d.attrs {
                        match attr {
                            a::AccountAttr::Simple(s) => match s.as_str() {
                                "signer" => acc.is_signer = true,
                                "writable" => acc.is_writable = true,
                                "readonly" => acc.is_writable = false,
                                "program" => acc.is_program = true,
                                _ => acc.account_type = Some(s.clone()),
                            },
                            a::AccountAttr::Type(t) => acc.account_type = Some(t.clone()),
                            a::AccountAttr::Authority(x) => acc.authority = Some(x.clone()),
                            a::AccountAttr::Pda(seeds) => acc.pda_seeds = Some(seeds.clone()),
                        }
                    }
                    handler.accounts.push(acc);
                }
            }
            a::HandlerClause::Requires { guard, on_fail } => {
                handler.requires.push(ParsedRequires {
                    lean_expr: expr_to_lean(&guard.node, Ctx::Guard, consts, env),
                    rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts),
                    error_name: on_fail.clone(),
                });
            }
            a::HandlerClause::Ensures(e) => {
                handler.ensures.push(ParsedEnsures {
                    lean_expr: expr_to_lean(&e.node, Ctx::Ensures, consts, env),
                    rust_expr: expr_to_rust(&e.node, Ctx::Ensures, consts),
                });
            }
            a::HandlerClause::Modifies(fs) => {
                handler.modifies = Some(fs.clone());
            }
            a::HandlerClause::Let { name, value } => {
                handler.let_bindings.push((
                    name.clone(),
                    expr_to_lean(&value.node, Ctx::Guard, consts, env),
                    expr_to_rust(&value.node, Ctx::Guard, consts),
                ));
            }
            a::HandlerClause::Effect(stmts) => {
                for Node { node: stmt, .. } in stmts {
                    handler.effects.push(render_effect(stmt, &params, consts));
                }
            }
            a::HandlerClause::Takes(fields) => {
                // Legacy sugar — append to takes_params.
                for f in fields {
                    handler
                        .takes_params
                        .push((f.name.clone(), type_ref_to_string(&f.ty)));
                }
            }
            a::HandlerClause::Transfers(clauses) => {
                for tc in clauses {
                    let amount = tc.amount.as_ref().map(|a| match a {
                        crate::ast::TransferAmount::Literal(v) => v.to_string(),
                        crate::ast::TransferAmount::Path(p) => {
                            // Pest captures amount as raw ident source — emit plain path.
                            let mut s = p.root.clone();
                            for seg in &p.segments {
                                match seg {
                                    crate::ast::PathSeg::Field(f) => {
                                        s.push('.');
                                        s.push_str(f);
                                    }
                                    crate::ast::PathSeg::Index(i) => {
                                        s.push('[');
                                        s.push_str(i);
                                        s.push(']');
                                    }
                                }
                            }
                            s
                        }
                    });
                    handler.transfers.push(crate::check::ParsedTransfer {
                        from: tc.from.clone(),
                        to: tc.to.clone(),
                        amount,
                        authority: tc.authority.clone(),
                    });
                }
            }
            a::HandlerClause::Emits(ev) => handler.emits.push(ev.clone()),
            a::HandlerClause::AbortsTotal => handler.aborts_total = true,
            a::HandlerClause::Permissionless => handler.permissionless = true,
            a::HandlerClause::Invariant(name) => handler.invariants.push(name.clone()),
            a::HandlerClause::Include(_) => {
                // Schema includes: forward-compat; ignored in phase 1.
            }
            a::HandlerClause::Match(_) => {
                // Branches are expanded into synthetic handlers by
                // `expand_handler`; this function only builds the shared
                // base and must ignore the branch clause itself.
            }
            a::HandlerClause::Call(c) => {
                // Split `Interface.handler` from the qualified path. Longer
                // paths (unusual — e.g. nested namespacing) flatten with '.'
                // into the handler name so the call still records, and the
                // resolver (slice 4+) can decide what to do.
                let segs = &c.target.0;
                let (iface, handler_name) = match segs.as_slice() {
                    [] => (String::new(), String::new()),
                    [only] => (String::new(), only.clone()),
                    [head, tail @ ..] => (head.clone(), tail.join(".")),
                };
                let args = c
                    .args
                    .iter()
                    .map(|arg| ParsedCallArg {
                        name: arg.name.clone(),
                        lean_expr: expr_to_lean(&arg.value.node, Ctx::Guard, consts, env),
                        rust_expr: expr_to_rust(&arg.value.node, Ctx::Guard, consts),
                    })
                    .collect();
                handler.calls.push(ParsedCall {
                    target_interface: iface,
                    target_handler: handler_name,
                    args,
                });
            }
        }
    }

    handler
}

// ----------------------------------------------------------------------------
// Interface adaptation
// ----------------------------------------------------------------------------

fn adapt_interface<'a>(
    iface: &'a a::InterfaceDecl,
    consts: ConstTable<'a>,
    env: &TypeEnv<'a>,
) -> ParsedInterface {
    let handlers = iface
        .handlers
        .iter()
        .map(|h| adapt_interface_handler(h, consts, env))
        .collect();
    ParsedInterface {
        name: iface.name.clone(),
        doc: iface.doc.clone(),
        program_id: iface.program_id.clone(),
        upstream: iface.upstream.as_ref().map(|u| ParsedUpstream {
            package: u.package.clone(),
            version: u.version.clone(),
            source: u.source.clone(),
            binary_hash: u.binary_hash.clone(),
            idl_hash: u.idl_hash.clone(),
            verified_with: u.verified_with.clone(),
            verified_at: u.verified_at.clone(),
        }),
        handlers,
    }
}

fn adapt_interface_handler<'a>(
    h: &'a a::InterfaceHandlerDecl,
    consts: ConstTable<'a>,
    env: &TypeEnv<'a>,
) -> ParsedInterfaceHandler {
    let mut out = ParsedInterfaceHandler {
        name: h.name.clone(),
        doc: h.doc.clone(),
        params: h
            .params
            .iter()
            .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
            .collect(),
        discriminant: None,
        accounts: Vec::new(),
        requires: Vec::new(),
        ensures: Vec::new(),
    };

    for Node { node: clause, .. } in &h.clauses {
        match clause {
            a::InterfaceHandlerClause::Discriminant(s) => {
                out.discriminant = Some(s.clone());
            }
            a::InterfaceHandlerClause::Accounts(descs) => {
                for d in descs {
                    let mut acc = ParsedHandlerAccount {
                        name: d.name.clone(),
                        is_signer: false,
                        is_writable: false,
                        is_program: false,
                        pda_seeds: None,
                        account_type: None,
                        authority: None,
                    };
                    for attr in &d.attrs {
                        match attr {
                            a::AccountAttr::Simple(s) => match s.as_str() {
                                "signer" => acc.is_signer = true,
                                "writable" => acc.is_writable = true,
                                "readonly" => acc.is_writable = false,
                                "program" => acc.is_program = true,
                                _ => acc.account_type = Some(s.clone()),
                            },
                            a::AccountAttr::Type(t) => acc.account_type = Some(t.clone()),
                            a::AccountAttr::Authority(x) => acc.authority = Some(x.clone()),
                            a::AccountAttr::Pda(seeds) => acc.pda_seeds = Some(seeds.clone()),
                        }
                    }
                    out.accounts.push(acc);
                }
            }
            a::InterfaceHandlerClause::Requires { guard, on_fail } => {
                out.requires.push(ParsedRequires {
                    lean_expr: expr_to_lean(&guard.node, Ctx::Guard, consts, env),
                    rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts),
                    error_name: on_fail.clone(),
                });
            }
            a::InterfaceHandlerClause::Ensures(e) => {
                out.ensures.push(ParsedEnsures {
                    lean_expr: expr_to_lean(&e.node, Ctx::Ensures, consts, env),
                    rust_expr: expr_to_rust(&e.node, Ctx::Ensures, consts),
                });
            }
        }
    }

    out
}

// ============================================================================
// Tests — parity with pest on percolator.qedspec
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const PERCOLATOR_SPEC: &str =
        include_str!("../../../examples/rust/percolator/percolator.qedspec");

    // Structural smoke test — percolator produces the shape we expect.
    // When pest existed this compared parser-for-parser; now it's a
    // regression fence against future adapter changes.
    #[test]
    fn percolator_shape() {
        let spec = parse_str(PERCOLATOR_SPEC).expect("chumsky parse");
        // 14 plain handlers + `liquidate` expanded into 3 branch arms = 17.
        assert_eq!(spec.handlers.len(), 17);
        assert_eq!(spec.properties.len(), 3);
        assert_eq!(spec.covers.len(), 2);
        assert_eq!(spec.liveness_props.len(), 1);

        let deposit = spec.handlers.iter().find(|h| h.name == "deposit").unwrap();
        assert_eq!(deposit.requires.len(), 2);
        assert_eq!(
            deposit.requires[0].error_name,
            Some("SlotInactive".to_string())
        );

        // Const substitution in guards: MAX_VAULT_TVL should be inlined.
        assert!(deposit.requires[1].lean_expr.contains("10000000000000000"));
    }

    // B1 regression: ADTs with multiple variants sharing the same field
    // names must produce a SINGLE entry per field (first-variant wins), not
    // a struct with N copies of each field.
    #[test]
    fn adt_variants_with_shared_fields_deduplicate() {
        let src = r#"spec T
type Battle
  | Active  of { pool : U64, status : U8 }
  | Frozen  of { pool : U64, status : U8 }
  | Settled of { pool : U64, status : U8 }
"#;
        let spec = parse_str(src).expect("parse");
        assert_eq!(spec.account_types.len(), 1);
        let at = &spec.account_types[0];
        assert_eq!(at.name, "Battle");
        // Pre-fix: fields.len() == 6 (3 variants × 2 fields, flattened).
        assert_eq!(
            at.fields.len(),
            2,
            "shared-field variants must dedupe to 2 fields, got {:?}",
            at.fields
        );
        let names: Vec<&str> = at.fields.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["pool", "status"]);
        // Lifecycle retains every variant name (Active/Frozen/Settled) for
        // Status enum generation.
        assert_eq!(at.lifecycle, vec!["Active", "Frozen", "Settled"]);
    }

    // B12 regression: property bodies referencing `state.x` must render as
    // `s.x` in the Rust form — `s` is the function parameter that
    // `emit_property_predicates` binds. Pre-v2.6.1 the Rust form was
    // `state.x >= 0`, which failed to compile (`cannot find value 'state'`).
    #[test]
    fn property_state_root_renders_as_s_in_rust() {
        let src = r#"spec T
state { x : U64 }
property x_bounded :
  state.x >= 0
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "x_bounded")
            .expect("property");
        let rust = prop.rust_expression.as_deref().expect("rust rendering");
        assert!(
            rust.contains("s.x"),
            "state.x should render as s.x, got: {}",
            rust
        );
        assert!(
            !rust.contains("state."),
            "no residual `state.` prefix in rust form: {}",
            rust
        );
    }

    // B2 regression: `implies` and `forall` must not leak Unicode symbols into
    // the Rust rendering of a property body.
    #[test]
    fn property_implies_renders_to_valid_rust() {
        let src = r#"spec T
state { x : U8 }
property implies_case :
  state.x == 2 implies state.x >= 2
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "implies_case")
            .expect("property");
        let rust = prop.rust_expression.as_deref().expect("rust rendering");
        // No lingering Lean arrows that would mojibake as `â` in downstream Rust.
        assert!(!rust.contains('\u{2192}'), "rust form has → : {}", rust);
        // Explicit desugaring check: `implies` must lower to `!(…) || (…)`.
        assert!(rust.contains("!("), "expected negation in: {}", rust);
        assert!(rust.contains("||"), "expected disjunction in: {}", rust);
        assert!(
            !crate::check::rust_expr_is_unsupported(rust),
            "implies should lower, not be marked unsupported: {}",
            rust
        );
    }

    #[test]
    fn property_forall_marked_unsupported_in_rust() {
        let src = r#"spec T
state { x : U8 }
property forall_case :
  forall v : U8, v >= 0
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "forall_case")
            .expect("property");
        let rust = prop.rust_expression.as_deref().expect("rust rendering");
        assert!(
            crate::check::rust_expr_is_unsupported(rust),
            "forall body should carry the unsupported marker: {}",
            rust
        );
        // The marker is wrapped in `/* ... */` so downstream emission puts it
        // inside a comment — no mojibake can leak into compiled Rust.
        assert!(
            rust.trim_start().starts_with("/*"),
            "marker must be a Rust block comment: {}",
            rust
        );
        assert!(
            rust.trim_end().ends_with("*/"),
            "marker must close the comment: {}",
            rust
        );
        assert!(
            !rust.contains('\u{2200}'),
            "rust must not contain ∀: {}",
            rust
        );
    }
}
