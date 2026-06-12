//! Adapter: typed AST (`ast::Spec`) → string-rendered `ParsedSpec` for
//! downstream consumers (check, lean_gen, kani, proptest_gen, …).
//!
//! Guard expressions are rendered to Lean-form (unicode operators, pre/post
//! state prefixes) and Rust-form (ASCII) strings here. The typed AST keeps
//! structure; the string forms are lossy projections.

use crate::ast::{self as a, Expr, Node, TopItem};
use crate::check::{
    FlowKind, ParsedAccountType, ParsedCall, ParsedCallArg, ParsedCover, ParsedEnsures,
    ParsedEnvironment, ParsedErrorCode, ParsedEvent, ParsedGuard, ParsedHandler,
    ParsedHandlerAccount, ParsedImport, ParsedInstruction, ParsedInterface, ParsedInterfaceHandler,
    ParsedLayoutField, ParsedLiveness, ParsedPda, ParsedProperty, ParsedPubkey, ParsedRecordType,
    ParsedRequires, ParsedSbpfProperty, ParsedSpec, ParsedStateBinder, ParsedSumType,
    ParsedUpstream, ParsedVariant, SbpfPropertyKind,
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
///   - `aliases`: type-alias name → its target rendered as a source-DSL string
///     (e.g. `AccountIdx` → `Fin[MAX_ACCOUNTS]`). Lets the quantifier renderer
///     resolve a binder type written as an alias down to the underlying
///     `Fin[N]` so it can emit a bounded `(0..N).all/.any(…)` iteration.
#[derive(Default, Clone)]
struct TypeEnv<'a> {
    state_fields: std::collections::BTreeMap<String, &'a a::TypeRef>,
    records: std::collections::BTreeMap<String, std::collections::BTreeMap<String, &'a a::TypeRef>>,
    params: Vec<(String, &'a a::TypeRef)>,
    aliases: std::collections::BTreeMap<String, String>,
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
                TopItem::TypeAlias(ta) => {
                    env.aliases
                        .insert(ta.name.clone(), type_ref_to_string(&ta.target));
                }
                // Ghosts render as state fields: `state.<ghost>` must resolve
                // in properties / invariants / `requires` / `ensures` and in
                // other ghosts' update RHS. They are rendering-only here — the
                // on-chain codegen reads `ParsedSpec.state_fields`, which never
                // includes ghosts.
                TopItem::Ghost(g) => {
                    env.state_fields.entry(g.name.clone()).or_insert(&g.ty);
                }
                _ => {}
            }
        }
        env
    }

    /// If `binder_ty` is a bounded index domain — either `Fin[N]` written
    /// directly or an alias resolving to one (e.g. `AccountIdx`) — return
    /// the bound symbol `N` (a numeric literal or a `const` name). Returns
    /// `None` for any non-`Fin` binder type. The bound is emitted verbatim
    /// by callers: a numeric literal renders as-is, a const name renders as
    /// the Rust `const` the codegen already emits.
    fn fin_bound(&self, binder_ty: &str) -> Option<String> {
        let resolved = self
            .aliases
            .get(binder_ty.trim())
            .map(String::as_str)
            .unwrap_or(binder_ty)
            .trim();
        let inner = resolved.strip_prefix("Fin[")?.strip_suffix(']')?;
        Some(inner.trim().to_string())
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
            // `if c then a else b` — both branches must agree; in phase 1
            // we trust the type checker and use the then-branch's kind.
            Expr::IfThenElse { then_branch, .. } => self.infer(&then_branch.node),
        }
    }

    /// True iff this Path resolves to a state/record field whose type would
    /// be lowered to a Quasar Pod companion (`U16`/`U32`/`U64`/`U128` →
    /// `PodU16`/…/`PodU128`; `I16`/…/`I128` → `PodI16`/…; `Bool` →
    /// `PodBool`). `U8`/`I8` stay native (alignment 1 already), so they
    /// don't need `.get()` and are reported as not Pod.
    ///
    /// Only state-rooted paths apply — handler parameters arrive at the
    /// inner handler in their native form (the dispatch shim unwraps
    /// `PodU64` → `u64` etc.) so a bare-ident param load isn't Pod.
    fn path_is_pod_field(&self, p: &a::Path) -> bool {
        if p.root != "state" {
            return false;
        }
        let Some(t) = self.path_type_ref(p) else {
            return false;
        };
        match t {
            a::TypeRef::Named(n) => matches!(
                n.as_str(),
                "U16" | "U32" | "U64" | "U128" | "I16" | "I32" | "I64" | "I128" | "Bool"
            ),
            _ => false,
        }
    }

    /// Resolve the leaf TypeRef of a Path, walking through state fields,
    /// records, and Map subscripts. Mirrors `path_kind` but returns the
    /// raw `TypeRef` instead of collapsing to `Kind`. Bare-ident params
    /// resolve through `params`.
    fn path_type_ref(&self, p: &a::Path) -> Option<&'a a::TypeRef> {
        if p.root == "state" {
            let mut current: Option<&a::TypeRef> = None;
            for seg in &p.segments {
                match seg {
                    a::PathSeg::Field(f) => {
                        let next = match current {
                            None => self.state_fields.get(f).copied(),
                            Some(a::TypeRef::Named(rec)) => {
                                self.records.get(rec).and_then(|m| m.get(f).copied())
                            }
                            Some(a::TypeRef::Map { inner, .. }) => match inner.as_ref() {
                                a::TypeRef::Named(rec) => {
                                    self.records.get(rec).and_then(|m| m.get(f).copied())
                                }
                                _ => None,
                            },
                            _ => None,
                        };
                        current = next;
                    }
                    a::PathSeg::Index(_) => {
                        if let Some(a::TypeRef::Map { inner, .. }) = current {
                            current = Some(inner.as_ref());
                        }
                    }
                }
            }
            return current;
        }
        if p.segments.is_empty() {
            return self
                .params
                .iter()
                .find(|(n, _)| n == &p.root)
                .map(|(_, t)| *t);
        }
        None
    }
}

/// Render typed expression to a Lean-compatible string (unicode operators).
/// Threads a `TypeEnv` through so arithmetic/comparison can promote Nat→Int
/// when operands' kinds differ.
fn expr_to_lean(e: &Expr, ctx: Ctx, consts: ConstTable, env: &TypeEnv) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        // Lean 4 Bool literals are lowercase `true`/`false`; `True`/`False`
        // are *Props*, so `flag := True` would type-error when `flag : Bool`.
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
            // `now()` is an axiomatized symbolic timestamp: the support
            // library declares `axiom now : Nat` (in scope because
            // lean_gen.rs imports QEDGen.Solana).
            if func == "now" && args.is_empty() {
                return "now".to_string();
            }
            // `current_epoch()` resolves the same way — axiomatized at
            // `QEDGen.Solana.Valid.current_epoch : Nat`.
            if func == "current_epoch" && args.is_empty() {
                return "current_epoch".to_string();
            }
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
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => format!(
            "(if {} then {} else {})",
            expr_to_lean(&cond.node, ctx, consts, env),
            expr_to_lean(&then_branch.node, ctx, consts, env),
            expr_to_lean(&else_branch.node, ctx, consts, env),
        ),
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

/// Any `Expr::Old(_)` in the tree? Used by `classify_property_body` and the
/// `vacuous_property_lowering` lint. Mirrors the shape of
/// `quantifier::find_nested_quantifier` — same node set, different predicate.
pub(crate) fn expr_contains_old(node: &Node<Expr>) -> bool {
    match &node.node {
        Expr::Old(_) => true,
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => expr_contains_old(lhs) || expr_contains_old(rhs),
        Expr::Not(inner) | Expr::Paren(inner) => expr_contains_old(inner),
        Expr::Sum { body, .. } | Expr::Quant { body, .. } => expr_contains_old(body),
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => {
            expr_contains_old(a) || expr_contains_old(b) || expr_contains_old(d)
        }
        Expr::Match { scrutinee, arms } => {
            expr_contains_old(scrutinee) || arms.iter().any(|arm| expr_contains_old(&arm.body))
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_contains_old(cond)
                || expr_contains_old(then_branch)
                || expr_contains_old(else_branch)
        }
        Expr::Let { value, body, .. } => expr_contains_old(value) || expr_contains_old(body),
        Expr::App { args, .. } => args.iter().any(expr_contains_old),
        Expr::Field { base, .. } => expr_contains_old(base),
        Expr::RecordLit(fs) => fs.iter().any(|(_, v)| expr_contains_old(v)),
        Expr::RecordUpdate { base, updates } => {
            expr_contains_old(base) || updates.iter().any(|(_, v)| expr_contains_old(v))
        }
        Expr::Ctor { payload, .. } => payload.as_ref().is_some_and(|p| expr_contains_old(p)),
        Expr::IsVariant { scrutinee, .. } => expr_contains_old(scrutinee),
        // Leaves
        Expr::Int(_) | Expr::Bool(_) | Expr::Path(_) => false,
    }
}

/// Temporal shape of a property body: contains `Expr::Old(_)` ⇒ `Binary`,
/// else `Unary`. Drives codegen dispatch ([`crate::check::PropertyClass`]).
pub(crate) fn classify_property_body(node: &Node<Expr>) -> crate::check::PropertyClass {
    if expr_contains_old(node) {
        crate::check::PropertyClass::Binary
    } else {
        crate::check::PropertyClass::Unary
    }
}

/// Lowering mode for state-path rendering in property bodies. `Binary` is
/// set by `proptest_gen` / `kani` when rendering a `PropertyClass::Binary`
/// body, matching the per-handler preservation harness that captures
/// pre-state before the handler call. Mirrors the Lean side's
/// `Ctx::Ensures` + `inside_old` distinction in `path_to_lean`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StateMode {
    /// `state.x` and `old(state.x)` both render to `s.x` — correct for
    /// single-state contexts. Default on every callsite.
    Unary,
    /// `state.x` → `post.x`, `old(state.x)` → `pre.x`. Used only when
    /// emitting `PropertyClass::Binary` property fn bodies.
    Binary,
}

/// Per-render options for `expr_to_rust`. `pod_aware` is set for the Quasar
/// target, where state/record integer fields lower to Pod companions and
/// need `.get()` on access. `state_mode` selects unary vs binary state-path
/// lowering ([`StateMode`]); `inside_old` tracks descent into an `old(...)`
/// subexpression so nested state refs render against pre-state.
#[derive(Copy, Clone)]
struct RustOpts<'a, 'env> {
    pod_aware: bool,
    env: &'a TypeEnv<'env>,
    state_mode: StateMode,
    inside_old: bool,
}

impl<'a, 'env> RustOpts<'a, 'env> {
    /// Return a copy with `inside_old = true`. Used when descending into
    /// `Expr::Old(_)` so nested state-path renders see the pre-state
    /// prefix.
    fn with_inside_old(self) -> Self {
        RustOpts {
            inside_old: true,
            ..self
        }
    }

    /// Copy with the given `state_mode` (Binary when rendering a
    /// `PropertyClass::Binary` property body).
    fn with_state_mode(self, state_mode: StateMode) -> Self {
        RustOpts { state_mode, ..self }
    }
}

/// `RustOpts` matching the legacy non-Pod-aware behavior. Used for the
/// `rust_expr` field that codegen consumes when emitting for Anchor (or
/// for any consumer that expects native Rust integer types).
fn opts_native<'a, 'env>(env: &'a TypeEnv<'env>) -> RustOpts<'a, 'env> {
    RustOpts {
        pod_aware: false,
        env,
        state_mode: StateMode::Unary,
        inside_old: false,
    }
}

/// `RustOpts` for the Pod-aware companion field (`rust_expr_pod`). Used
/// when codegen is emitting for Quasar.
fn opts_pod<'a, 'env>(env: &'a TypeEnv<'env>) -> RustOpts<'a, 'env> {
    RustOpts {
        pod_aware: true,
        env,
        state_mode: StateMode::Unary,
        inside_old: false,
    }
}

/// Render typed expression to a Rust-compatible string (ASCII operators).
fn expr_to_rust(e: &Expr, ctx: Ctx, consts: ConstTable, opts: RustOpts<'_, '_>) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Path(p) => render_path_with_pod(p, ctx, consts, opts),
        // `old(...)` routes through `opts.inside_old`: the path renderer
        // emits `pre.x` (Binary mode) instead of `post.x`; non-Path inner
        // exprs render recursively with the flag set (a comment-form
        // lowering would be invalid Rust in expression position).
        Expr::Old(inner) => expr_to_rust(&inner.node, ctx, consts, opts.with_inside_old()),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => format!(
            "sum_over::<{}>(|{}| {})",
            binder_ty,
            binder,
            expr_to_rust(&body.node, ctx, consts, opts)
        ),
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => {
            // A quantifier over a bounded domain lowers to an exhaustive
            // `RangeInclusive::all` (forall) / `any` (exists) — correct and
            // cheap for test suites. `Fin[N]` index domains iterate `0..N`;
            // small integers (U8/I8) exhaust their full range. Wider integer
            // domains can't be exhausted in a test loop, so the sentinel
            // tells the caller to skip or escalate to harness-level lowering.
            let method = match kind {
                a::Quantifier::Forall => "all",
                a::Quantifier::Exists => "any",
            };
            let body_rust = expr_to_rust(&body.node, ctx, consts, opts);
            // `exists` over a bounded index domain (`Fin[N]`, directly or via
            // an alias): iterate `0..N` with `.any(…)` — a real, non-vacuous
            // predicate usable wherever a bool is expected. `forall` over
            // `Fin[N]` deliberately does NOT take this path: it keeps the
            // per-slot lowering (`{prop}_at`) so a preserved-property
            // assertion checks the one modified slot rather than unwinding a
            // whole-array loop in Kani. Existence has no per-slot analogue.
            if matches!(kind, a::Quantifier::Exists) {
                if let Some(bound) = opts.env.fin_bound(binder_ty) {
                    return format!("(0..({} as usize)).any(|{}| {})", bound, binder, body_rust);
                }
            }
            // Small integer domains (U8, I8) can be exhausted directly (256
            // iterations max).
            let rust_ty = match binder_ty.as_str() {
                "U8" => Some("u8"),
                "I8" => Some("i8"),
                _ => None,
            };
            let Some(rust_ty) = rust_ty else {
                let kind_name = match kind {
                    a::Quantifier::Forall => "forall",
                    a::Quantifier::Exists => "exists",
                };
                return format!(
                    "/* QEDGEN_UNSUPPORTED_QUANTIFIER: {} {} : {} — lower at harness level */",
                    kind_name, binder, binder_ty
                );
            };
            format!(
                "({}::MIN..={}::MAX).{}(|{}| {})",
                rust_ty, rust_ty, method, binder, body_rust
            )
        }
        Expr::BoolOp { op, lhs, rhs } => {
            let lhs_r = expr_to_rust(&lhs.node, ctx, consts, opts);
            let rhs_r = expr_to_rust(&rhs.node, ctx, consts, opts);
            match op {
                a::BoolOp::And => format!("({}) && ({})", lhs_r, rhs_r),
                a::BoolOp::Or => format!("({}) || ({})", lhs_r, rhs_r),
                // `a implies b` ≡ `!a || b`; parenthesize both sides to survive
                // surrounding precedence (matters once callers compose via `&&`/`||`).
                a::BoolOp::Implies => format!("(!({})) || ({})", lhs_r, rhs_r),
            }
        }
        Expr::Not(inner) => format!("!({})", expr_to_rust(&inner.node, ctx, consts, opts)),
        Expr::Cmp { op, lhs, rhs } => {
            let sym = match op {
                a::CmpOp::Eq => "==",
                a::CmpOp::Ne => "!=",
                a::CmpOp::Le => "<=",
                a::CmpOp::Ge => ">=",
                a::CmpOp::Lt => "<",
                a::CmpOp::Gt => ">",
            };
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
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
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
            format!("{}{}{}", l_str, sym, r_str)
        }
        Expr::Paren(inner) => format!("({})", expr_to_rust(&inner.node, ctx, consts, opts)),
        // mul_div_{floor,ceil}_u128 are u128-typed helpers (the intermediate
        // `a * b` can overflow u64 even when both operands are u64-bounded).
        // Inside arbitrary expression contexts (`requires` / `ensures` /
        // `effect` RHS) the u128 width is intentional — the spec author
        // may compare against a u128 literal (e.g. percolator's `…
        // mul_div_floor(...) <= 100000000000000000000`). The let-binding
        // emit site (see `HandlerClause::Let` handler below) narrows back
        // to U64 explicitly when the spec writes `let X = mul_div_*(…)`,
        // because the binding's spec-declared type is U64 and downstream
        // U64 uses (e.g. `total - X`) need to typecheck.
        Expr::MulDivFloor { a, b, d } => format!(
            "mul_div_floor_u128({}, {}, {})",
            render_helper_arg(&a.node, ctx, consts, opts),
            render_helper_arg(&b.node, ctx, consts, opts),
            render_helper_arg(&d.node, ctx, consts, opts)
        ),
        Expr::MulDivCeil { a, b, d } => format!(
            "mul_div_ceil_u128({}, {}, {})",
            render_helper_arg(&a.node, ctx, consts, opts),
            render_helper_arg(&b.node, ctx, consts, opts),
            render_helper_arg(&d.node, ctx, consts, opts)
        ),
        Expr::Match { scrutinee, arms } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            let mut out = format!("match {} {{", sc);
            for arm in arms {
                out.push_str(&format!("\n    {}::{}", "/* ty */", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push_str(&format!("({})", b));
                }
                out.push_str(" => ");
                out.push_str(&expr_to_rust(&arm.body.node, ctx, consts, opts));
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
                expr_to_rust(&p.node, ctx, consts, opts)
            ),
        },
        Expr::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts, opts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {} }}", "/* ty */", body)
        }
        Expr::RecordUpdate { base, updates } => {
            let base_str = expr_to_rust(&base.node, ctx, consts, opts);
            let body = updates
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts, opts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {}, ..{} }}", "/* ty */", body, base_str)
        }
        Expr::IsVariant { scrutinee, variant } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            format!("matches!({}, {}::{}(..))", sc, "/* ty */", variant)
        }
        Expr::App { func, args } => {
            // `now()` lowers to the on-chain clock read. `unwrap()` rather
            // than `?` so the expression is valid in assertion / property
            // bodies (the surrounding fn may not return Result); Clock is a
            // sysvar that always succeeds in practice. The i64→u64 cast is
            // sign-bit-preserving; negative unix_timestamp doesn't happen
            // on chain.
            if func == "now" && args.is_empty() {
                return "(solana_program::clock::Clock::get().unwrap().unix_timestamp as u64)"
                    .to_string();
            }
            // `current_epoch()` reads `.epoch` (already u64) — no cast.
            if func == "current_epoch" && args.is_empty() {
                return "solana_program::clock::Clock::get().unwrap().epoch".to_string();
            }
            let args_str: Vec<String> = args
                .iter()
                .map(|n| expr_to_rust(&n.node, ctx, consts, opts))
                .collect();
            format!("{}({})", func, args_str.join(", "))
        }
        Expr::Field { base, field } => {
            let base_str = expr_to_rust(&base.node, ctx, consts, opts);
            format!("{}.{}", base_str, field)
        }
        Expr::Let { name, value, body } => {
            // Rust lowers a let-in expression to a block. Parentheses are
            // safe around the block for embedding in larger expressions.
            format!(
                "({{ let {} = {}; {} }})",
                name,
                expr_to_rust(&value.node, ctx, consts, opts),
                expr_to_rust(&body.node, ctx, consts, opts)
            )
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => format!(
            "(if {} {{ {} }} else {{ {} }})",
            expr_to_rust(&cond.node, ctx, consts, opts),
            expr_to_rust(&then_branch.node, ctx, consts, opts),
            expr_to_rust(&else_branch.node, ctx, consts, opts),
        ),
    }
}

/// Render a Path, applying a `.get()` postfix when it resolves to a
/// Pod-flavored field on Quasar (`pod_aware`). Non-Pod fields (`u8`/`i8`/
/// `Bool` already alignment 1, paths into non-state types) pass through.
fn render_path_with_pod(
    p: &a::Path,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
) -> String {
    let base = path_to_rust(p, ctx, consts, opts);
    if opts.pod_aware && opts.env.path_is_pod_field(p) {
        format!("{}.get()", base)
    } else {
        base
    }
}

/// Rust-flavor kind inference: mostly the same as `TypeEnv::infer` but
/// `MulDivFloor` / `MulDivCeil` always report `Nat` because the codegen
/// lowers them to `mul_div_floor_u128` / `_ceil_u128` helpers that
/// return `u128`. Without this override the Lean-style inheritance
/// (`Int` if any operand is `Int`) bleeds the wrong type into Rust
/// comparisons against the helper's u128 result.
fn rust_infer_kind(env: &TypeEnv, e: &Expr) -> Kind {
    match e {
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } => Kind::Nat,
        Expr::Paren(inner) => rust_infer_kind(env, &inner.node),
        Expr::Old(inner) => rust_infer_kind(env, &inner.node),
        _ => env.infer(e),
    }
}

/// `true` iff `e` is a `mul_div_floor` / `mul_div_ceil` call, possibly
/// wrapped in `Paren` and/or `Old`. Mirrors the peel pattern in
/// `rust_infer_kind` above so the let-binding narrow gate stays in
/// lock-step — `let X = (mul_div_floor(...))` and `let X =
/// old(mul_div_floor(...))` both want the same narrowing as the bare
/// form.
fn is_mul_div_let_rhs(e: &Expr) -> bool {
    match e {
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } => true,
        Expr::Paren(inner) => is_mul_div_let_rhs(&inner.node),
        Expr::Old(inner) => is_mul_div_let_rhs(&inner.node),
        _ => false,
    }
}

/// Render both sides of a binary op, casting to `i128` when kinds mix.
/// Mirrors the Lean-side `render_binary_with_coercion`. The Nat→Int cast is
/// target-independent (Rust rejects `u128 + i128` everywhere) — do NOT gate
/// it on `pod_aware`, which is Quasar-only and would silently break Anchor
/// scaffolds mixing U128 + I128.
fn render_rust_binary_with_coercion(
    lhs: &Node<Expr>,
    rhs: &Node<Expr>,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
) -> (String, String) {
    let lk = rust_infer_kind(opts.env, &lhs.node);
    let rk = rust_infer_kind(opts.env, &rhs.node);
    let l = expr_to_rust(&lhs.node, ctx, consts, opts);
    let r = expr_to_rust(&rhs.node, ctx, consts, opts);
    // Widening Nat → Int must cast BOTH sides to the same wide type —
    // casting only the Nat side leaves `i64 >= i128`, which doesn't
    // typecheck. Symmetric i128 widening loses no precision.
    match (lk, rk) {
        (Kind::Nat, Kind::Int) => (format!("(({}) as i128)", l), format!("(({}) as i128)", r)),
        (Kind::Int, Kind::Nat) => (format!("(({}) as i128)", l), format!("(({}) as i128)", r)),
        _ => (l, r),
    }
}

/// `mul_div_{floor,ceil}_u128` take `u128` args; spec operands may be
/// U64 / I64 / I128 / native params. Cast unconditionally on every target
/// (gating on `pod_aware` would break Anchor) — `as u128` from u64 widens;
/// from i128 it truncates, matching the Lean side's Int → u128 lowering.
fn render_helper_arg(e: &Expr, ctx: Ctx, consts: ConstTable, opts: RustOpts<'_, '_>) -> String {
    let rendered = expr_to_rust(e, ctx, consts, opts);
    format!("(({}) as u128)", rendered)
}

fn path_to_rust(p: &a::Path, _ctx: Ctx, consts: ConstTable, opts: RustOpts<'_, '_>) -> String {
    let mut out = String::new();
    if p.segments.is_empty() && p.root != "state" {
        // Bare ident — substitute if declared as a const (pest parity).
        if let Some(v) = consts.get(&p.root) {
            return v.clone();
        }
    }
    // `state.X` lowers to `s.X` — every Rust consumer (property fn bodies,
    // transition-fn assume predicates, abort.rust_expr) binds state to `s`.
    // In Binary state_mode the prefix splits by `inside_old`:
    //   inside_old=true  → `pre.<field>`   (old(state.x))
    //   inside_old=false → `post.<field>`  (state.x)
    // Mirrors `path_to_lean`. Unary callers keep `s.<field>` regardless.
    if p.root == "state" {
        let prefix = match (opts.state_mode, opts.inside_old) {
            (StateMode::Unary, _) => "s",
            (StateMode::Binary, true) => "pre",
            (StateMode::Binary, false) => "post",
        };
        out.push_str(prefix);
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
                // Cast index expression to `usize`. A Map[N] T lowers to
                // `[T; N]`; the spec's index could be a u8/u16/u32/Fin
                // handler param, none of which Rust accepts directly as
                // an array index. The `as usize` cast is always safe (no
                // negative values reach this path — Fin/U* are unsigned).
                out.push('[');
                out.push('(');
                out.push_str(i);
                out.push_str(") as usize");
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
    // Check record and ADT variant fields for `Map[N] <name>` (value) OR
    // `Map[<name>] T` (enum used as key).
    fn type_ref_mentions(t: &a::TypeRef, name: &str) -> bool {
        match t {
            a::TypeRef::Map { inner, bound } => {
                let value_match = matches!(inner.as_ref(), a::TypeRef::Named(n) if n == name);
                // Key match: the bound is a raw ident string, resolved
                // later — a bare name match is the routing signal.
                let key_match = bound == name;
                value_match || key_match
            }
            _ => false,
        }
    }
    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Record(r) => {
                for f in &r.fields {
                    if type_ref_mentions(&f.ty, name) {
                        return true;
                    }
                }
            }
            TopItem::Adt(adt) => {
                for v in &adt.variants {
                    for f in &v.fields {
                        if type_ref_mentions(&f.ty, name) {
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

// ============================================================================
// Effect rendering: (field_name, op, value_string)
// ============================================================================

/// Render an `EffectStmt` to the `(field, op, value)` triple consumed by
/// every backend, plus the per-site error-variant override codegen reads
/// when lowering checked `+=` / `-=`. Override is always `None` for
/// non-checked ops — the parser is permissive for error positioning; the
/// adapter normalizes.
fn render_effect(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
) -> ((String, String, String), Option<String>) {
    // Field name: preserve subscript syntax as-is (e.g., `accounts[i].capital`).
    // Both Lean and Rust consumers read this string; Rust-side `as usize`
    // index casting is applied at the codegen.rs::mechanize_effect site
    // so the Lean output stays untouched.
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
    // Per-effect semantic tag:
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
    // Keep the per-site override only for ops that can fail (checked
    // Add / Sub); saturating / wrapping / Set can't trigger an error
    // variant, so drop any parser-captured override here.
    let on_error = match stmt.op {
        a::EffectOp::Add | a::EffectOp::Sub => stmt.on_error.clone(),
        _ => None,
    };
    ((field, op.to_string(), value), on_error)
}

/// Best-effort reconstruction of a `TypeRef` from its rendered string form,
/// used only inside `render_effect` where we don't have the original AST.
fn string_to_typeref_best_effort(s: &str) -> a::TypeRef {
    a::TypeRef::Named(s.trim().to_string())
}

/// Render an effect RHS to the same string form `render_effect` uses —
/// factored out for the variant-promotion desugaring. Mirrors
/// `render_effect`'s value branch exactly.
fn render_effect_rhs_value(rhs: &Expr, params: &[(String, String)], consts: ConstTable) -> String {
    match rhs {
        Expr::Int(v) => v.to_string(),
        Expr::Path(p) => {
            let is_param = p.segments.is_empty() && params.iter().any(|(n, _)| n == &p.root);
            if is_param {
                p.root.clone()
            } else if p.root == "state" {
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
        other => {
            let env = TypeEnv::default().with_params(&[]);
            let _ = params;
            expr_to_lean(other, Ctx::Guard, consts, &env)
        }
    }
}

/// Desugar `state := .Variant { f := e, ... }` whole-state assignment into
/// per-field effects with variant-prefixed LHS (`Variant.f`), routing the
/// shape through the existing `emit_cross_variant_promotion` emitter. Other
/// shapes return a single-element Vec of `render_effect` output (uniform
/// iteration). Unit-variant promotion (`state := .Closed`) returns an empty
/// Vec — the wrapper assignment handles the transition from
/// `handler.post_status`.
fn render_effect_or_expand_variant_promotion(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
) -> Vec<((String, String, String), Option<String>)> {
    if matches!(stmt.op, a::EffectOp::Set)
        && stmt.lhs.root == "state"
        && stmt.lhs.segments.is_empty()
    {
        if let Expr::Ctor { variant, payload } = &stmt.rhs.node {
            match payload {
                None => {
                    // Unit variant — drop. Wrapper handles transition.
                    return Vec::new();
                }
                Some(p) => {
                    if let Expr::RecordLit(fields) = &p.node {
                        // Payload variant + record literal — expand
                        // per field with variant-prefixed LHS.
                        return fields
                            .iter()
                            .map(|(fname, fvalue)| {
                                let lhs_str = format!("{}.{}", variant, fname);
                                let value_str =
                                    render_effect_rhs_value(&fvalue.node, params, consts);
                                ((lhs_str, "set".to_string(), value_str), None)
                            })
                            .collect();
                    }
                    // Non-record-literal payload (e.g.
                    // `state := .Active some_bound_record`) — fall
                    // through to single render_effect. Codegen will
                    // bail (currently unsupported); agent fills the
                    // todo!() body.
                }
            }
        }
    }
    vec![render_effect(stmt, params, consts)]
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
    let parsed = adapt(&typed);
    typecheck_spec(&typed, &parsed)?;
    Ok(parsed)
}

/// Walk every guard / ensures / effect-RHS / property body and collect
/// every `Expr::App` call site as an uninterpreted helper. First encounter
/// wins for the signature; duplicates (same name + arity) are skipped.
///
/// Return type is always boolean — every App call in the DSL lives in a
/// boolean-valued position. A call in an arithmetic position (`effect
/// { x := foo(y) + 1 }`) won't typecheck against the emitted axiom; richer
/// context-sensitive inference is future work.
fn collect_uninterpreted_helpers(
    spec: &a::Spec,
    parsed: &ParsedSpec,
) -> Vec<(String, Vec<String>, String)> {
    let field_types = collect_field_types(parsed);
    let mut out: Vec<(String, Vec<String>, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();

    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Handler(h) => {
                let param_types: std::collections::HashMap<String, String> = h
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                    .collect();
                for Node { node: clause, .. } in &h.clauses {
                    match clause {
                        a::HandlerClause::Requires { guard, .. } => {
                            walk_apps(&guard.node, &field_types, &param_types, &mut out, &mut seen);
                        }
                        a::HandlerClause::Ensures(e) => {
                            walk_apps(&e.node, &field_types, &param_types, &mut out, &mut seen);
                        }
                        a::HandlerClause::Effect(blocks) => {
                            // Flatten through `match` arms.
                            for stmt in a::flatten_effect_blocks(blocks) {
                                walk_apps(
                                    &stmt.rhs.node,
                                    &field_types,
                                    &param_types,
                                    &mut out,
                                    &mut seen,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            TopItem::Property(p) => {
                walk_apps(
                    &p.body.node,
                    &field_types,
                    &std::collections::HashMap::new(),
                    &mut out,
                    &mut seen,
                );
            }
            // Walk ref_impl bodies too: a helper called only from a
            // ref_impl body must enter the uninterpreted-helper bag or Lean
            // fails on the unresolved name. The post-walk filter strips
            // names that are themselves ref_impls so declarations don't
            // collide.
            TopItem::RefImpl(r) => {
                let param_types: std::collections::HashMap<String, String> = r
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                    .collect();
                walk_apps(
                    &r.body.node,
                    &field_types,
                    &param_types,
                    &mut out,
                    &mut seen,
                );
            }
            _ => {}
        }
    }
    out
}

fn walk_apps(
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    out: &mut Vec<(String, Vec<String>, String)>,
    seen: &mut std::collections::HashSet<(String, usize)>,
) {
    match expr {
        Expr::App { func, args } => {
            // Skip the `now()` builtin — it resolves via the support-library
            // axiom `QEDGen.Solana.Valid.now : Nat`; emitting `axiom now :
            // Bool` here would collide at elaboration.
            if func == "now" && args.is_empty() {
                return;
            }
            // `current_epoch()` — same support-library shape as `now`.
            if func == "current_epoch" && args.is_empty() {
                return;
            }
            let key = (func.clone(), args.len());
            if seen.insert(key) {
                let arg_types: Vec<String> = args
                    .iter()
                    .map(|n| infer_lean_type(&n.node, field_types, param_types))
                    .collect();
                // Bool, not Prop: requires/ensures lower to a transition
                // `if`-guard, which Lean needs `Decidable` — `axiom foo : T
                // → Prop` is opaque and fails to compile. `Bool` is
                // auto-`Decidable` and lifts into Prop via the `b = true`
                // coercion the call-site renderer already produces.
                out.push((func.clone(), arg_types, "Bool".to_string()));
            }
            for n in args {
                walk_apps(&n.node, field_types, param_types, out, seen);
            }
        }
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            walk_apps(&lhs.node, field_types, param_types, out, seen);
            walk_apps(&rhs.node, field_types, param_types, out, seen);
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            walk_apps(&inner.node, field_types, param_types, out, seen);
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            walk_apps(&body.node, field_types, param_types, out, seen);
        }
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => {
            walk_apps(&a.node, field_types, param_types, out, seen);
            walk_apps(&b.node, field_types, param_types, out, seen);
            walk_apps(&d.node, field_types, param_types, out, seen);
        }
        _ => {}
    }
}

/// Best-effort Lean type for an argument expression. Used only for
/// axiom signature synthesis; a wrong guess degrades to a type error
/// at `lake build` time, but isn't silently corrupting anything.
fn infer_lean_type(
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
) -> String {
    match expr {
        Expr::Int(_) => "Nat".to_string(),
        Expr::Bool(_) => "Bool".to_string(),
        Expr::Path(p) => {
            let dsl_type = resolve_path_type(p, field_types, param_types);
            match dsl_type {
                Some("Pubkey") => "Pubkey".to_string(),
                Some("Bool") => "Bool".to_string(),
                Some(t) if is_signed_int(t) => "Int".to_string(),
                Some(_) => "Nat".to_string(),
                None => "Nat".to_string(),
            }
        }
        _ => "Nat".to_string(),
    }
}

fn is_signed_int(t: &str) -> bool {
    matches!(t, "I8" | "I16" | "I32" | "I64" | "I128")
}

/// Narrow check-time guard for Pubkey-vs-numeric-literal mismatches in
/// effect RHS and `requires` / `ensures` comparisons — the DSL has no
/// Pubkey literal syntax, so `state.key := 0` is always a category error.
/// Deliberately narrow: only fail when one side resolves to a Pubkey field
/// and the other is a bare integer literal.
pub fn typecheck_spec(spec: &a::Spec, parsed: &ParsedSpec) -> anyhow::Result<()> {
    let field_types = collect_field_types(parsed);
    let const_literals = collect_numeric_consts(spec);

    for Node { node, .. } in &spec.items {
        if let TopItem::Handler(h) = node {
            let param_types: std::collections::HashMap<String, String> = h
                .params
                .iter()
                .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                .collect();
            typecheck_handler(h, &field_types, &param_types, &const_literals)?;
        }
    }

    // Reject recursive `ref_impl`s (direct or mutual): Lean would emit a
    // non-terminating `def` and fail elaboration — surface at adapt time
    // with a fix-it pointing at structural decomposition.
    check_no_recursive_ref_impls(spec)?;

    Ok(())
}

/// Collect every function name referenced as `Expr::App { func, .. }`
/// anywhere in `expr`. Used by the ref_impl recursion checker — direct
/// and mutual recursion both manifest as a ref_impl body calling
/// another (or itself).
fn collect_app_funcs(expr: &Expr, out: &mut std::collections::HashSet<String>) {
    match expr {
        Expr::App { func, args } => {
            out.insert(func.clone());
            for n in args {
                collect_app_funcs(&n.node, out);
            }
        }
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            collect_app_funcs(&lhs.node, out);
            collect_app_funcs(&rhs.node, out);
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            collect_app_funcs(&inner.node, out);
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            collect_app_funcs(&body.node, out);
        }
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => {
            collect_app_funcs(&a.node, out);
            collect_app_funcs(&b.node, out);
            collect_app_funcs(&d.node, out);
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_app_funcs(&cond.node, out);
            collect_app_funcs(&then_branch.node, out);
            collect_app_funcs(&else_branch.node, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_app_funcs(&scrutinee.node, out);
            for arm in arms {
                collect_app_funcs(&arm.body.node, out);
            }
        }
        Expr::Let { value, body, .. } => {
            collect_app_funcs(&value.node, out);
            collect_app_funcs(&body.node, out);
        }
        Expr::Ctor {
            payload: Some(p), ..
        } => {
            collect_app_funcs(&p.node, out);
        }
        Expr::RecordLit(fields) => {
            for (_, v) in fields {
                collect_app_funcs(&v.node, out);
            }
        }
        Expr::RecordUpdate { base, updates } => {
            collect_app_funcs(&base.node, out);
            for (_, v) in updates {
                collect_app_funcs(&v.node, out);
            }
        }
        Expr::IsVariant { scrutinee, .. } => {
            collect_app_funcs(&scrutinee.node, out);
        }
        Expr::Field { base, .. } => {
            collect_app_funcs(&base.node, out);
        }
        _ => {}
    }
}

/// Reject recursive `ref_impl`s: build the call graph restricted to
/// ref_impl names and DFS-detect cycles. Direct (`r calls r`) and mutual
/// (`r → s → r`) recursion both fail with a fix-it.
fn check_no_recursive_ref_impls(spec: &a::Spec) -> anyhow::Result<()> {
    // Gather ref_impl names + their bodies.
    let mut ref_impls: Vec<(String, &Node<Expr>)> = Vec::new();
    for Node { node, .. } in &spec.items {
        if let TopItem::RefImpl(r) = node {
            ref_impls.push((r.name.clone(), &r.body));
        }
    }
    if ref_impls.is_empty() {
        return Ok(());
    }
    let ref_impl_names: std::collections::HashSet<String> =
        ref_impls.iter().map(|(n, _)| n.clone()).collect();

    // Build the per-impl set of called ref_impl names. Calls to
    // non-ref_impl functions (builtins, uninterpreted helpers,
    // mul_div_*) are ignored.
    let mut call_graph: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (name, body) in &ref_impls {
        let mut calls = std::collections::HashSet::new();
        collect_app_funcs(&body.node, &mut calls);
        let edges: Vec<String> = calls
            .into_iter()
            .filter(|f| ref_impl_names.contains(f))
            .collect();
        call_graph.insert(name.clone(), edges);
    }

    // DFS cycle detection: WHITE (unvisited), GRAY (on stack), BLACK
    // (fully explored). A back-edge to GRAY signals a cycle.
    enum Color {
        Gray,
        Black,
    }
    let mut color: std::collections::HashMap<String, Color> = std::collections::HashMap::new();

    fn visit(
        node: &str,
        graph: &std::collections::HashMap<String, Vec<String>>,
        color: &mut std::collections::HashMap<String, Color>,
        stack: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        color.insert(node.to_string(), Color::Gray);
        stack.push(node.to_string());
        if let Some(succs) = graph.get(node) {
            for next in succs {
                match color.get(next) {
                    Some(Color::Gray) => {
                        // Cycle. Find the start of the cycle in `stack`.
                        let cycle_start = stack.iter().position(|n| n == next).unwrap_or(0);
                        let mut cycle: Vec<&str> =
                            stack[cycle_start..].iter().map(|s| s.as_str()).collect();
                        cycle.push(next.as_str());
                        let chain = cycle.join(" -> ");
                        anyhow::bail!(
                            "recursive `ref_impl` not supported: {chain}\n\
                             v2.26 rejects direct and mutual recursion in `ref_impl` bodies.\n\
                             Fix: split into a non-recursive helper plus a state-bearing handler.\n\
                             Termination + Lean `def` lowering for recursive refs is a meta-question\n\
                             outside the LP-shape scope this construct targets."
                        );
                    }
                    Some(Color::Black) => continue,
                    None => visit(next, graph, color, stack)?,
                }
            }
        }
        stack.pop();
        color.insert(node.to_string(), Color::Black);
        Ok(())
    }

    for (name, _) in &ref_impls {
        if !color.contains_key(name) {
            let mut stack: Vec<String> = Vec::new();
            visit(name, &call_graph, &mut color, &mut stack)?;
        }
    }

    Ok(())
}

fn collect_numeric_consts(spec: &a::Spec) -> std::collections::HashMap<String, i128> {
    let mut out = std::collections::HashMap::new();
    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Const { name, value } => {
                out.insert(name.clone(), *value);
            }
            TopItem::Pragma(p) => {
                for Node { node, .. } in &p.items {
                    if let TopItem::Const { name, value } = node {
                        out.insert(name.clone(), *value);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Flatten every field declaration in the spec into `name → DSL-type`.
/// State fields, record fields, sum-variant payload fields, and
/// account-type fields all live in the same namespace from the DSL's
/// point of view — the same `state.key` can resolve against any of them.
fn collect_field_types(parsed: &ParsedSpec) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for (n, t) in &parsed.state_fields {
        out.insert(n.clone(), t.clone());
    }
    for rec in &parsed.records {
        for (n, t) in &rec.fields {
            out.insert(n.clone(), t.clone());
        }
    }
    for sum in &parsed.sum_types {
        for v in &sum.variants {
            for (n, t) in &v.fields {
                out.insert(n.clone(), t.clone());
            }
        }
    }
    for acct in &parsed.account_types {
        for (n, t) in &acct.fields {
            out.insert(n.clone(), t.clone());
        }
    }
    out
}

fn typecheck_handler(
    h: &a::HandlerDecl,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    for Node { node, .. } in &h.clauses {
        match node {
            a::HandlerClause::Effect(blocks) => {
                // Typecheck every leaf, including under match.
                for stmt in a::flatten_effect_blocks(blocks) {
                    check_effect_typed(&h.name, stmt, field_types, param_types, const_literals)?;
                }
            }
            a::HandlerClause::Requires { guard, .. } => {
                check_cmp_types(
                    &h.name,
                    "requires",
                    &guard.node,
                    field_types,
                    param_types,
                    const_literals,
                )?;
            }
            a::HandlerClause::Ensures(e) => {
                check_cmp_types(
                    &h.name,
                    "ensures",
                    &e.node,
                    field_types,
                    param_types,
                    const_literals,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolve the leaf field of a path like `state.key` or
/// `accounts[i].capital` to its DSL type, if declared.
fn resolve_path_type<'a>(
    p: &a::Path,
    field_types: &'a std::collections::HashMap<String, String>,
    param_types: &'a std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    // Walk the path to find the last `.field` segment — that's the leaf
    // whose declared type matters for assignment/comparison.
    let mut last_field: Option<&str> = None;
    for seg in &p.segments {
        if let a::PathSeg::Field(f) = seg {
            last_field = Some(f.as_str());
        }
    }
    match last_field {
        Some(name) => field_types.get(name).map(String::as_str),
        None => {
            // Bare root identifier — either a handler param or a state
            // field with no segments.
            param_types
                .get(&p.root)
                .map(String::as_str)
                .or_else(|| field_types.get(&p.root).map(String::as_str))
        }
    }
}

fn check_effect_typed(
    handler_name: &str,
    stmt: &a::EffectStmt,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    let lhs_type = match resolve_path_type(&stmt.lhs, field_types, param_types) {
        Some(t) => t,
        None => return Ok(()),
    };
    if lhs_type == "Pubkey" {
        if let Some(v) = numeric_literal_value(&stmt.rhs.node, const_literals) {
            anyhow::bail!(
                "handler `{}` effect `{} := {}`: Pubkey field cannot be assigned a numeric literal. \
                 The DSL has no Pubkey-literal syntax — use a handler parameter, a constant, \
                 or the spec's `program_id` as the source pubkey.",
                handler_name,
                render_path_human(&stmt.lhs),
                v
            );
        }
    }
    Ok(())
}

fn check_cmp_types(
    handler_name: &str,
    clause_kind: &str,
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    match expr {
        Expr::Cmp { lhs, rhs, .. } => {
            check_cmp_pair(
                handler_name,
                clause_kind,
                &lhs.node,
                &rhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
            // Cmp operands are terminal atoms in the DSL (no nested Cmp),
            // so no need to recurse into them.
        }
        Expr::BoolOp { lhs, rhs, .. } => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &lhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
            check_cmp_types(
                handler_name,
                clause_kind,
                &rhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &inner.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &body.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn check_cmp_pair(
    handler_name: &str,
    clause_kind: &str,
    lhs: &Expr,
    rhs: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    // Try both orientations (LHS Pubkey / RHS Int and vice versa).
    let pubkey_vs_int = |p: &Expr, i: &Expr| -> Option<(String, i128)> {
        let path = match p {
            Expr::Path(path) => path,
            _ => return None,
        };
        let t = resolve_path_type(path, field_types, param_types)?;
        if t != "Pubkey" {
            return None;
        }
        if let Some(v) = numeric_literal_value(i, const_literals) {
            return Some((render_path_human(path), v));
        }
        None
    };
    if let Some((path_str, v)) = pubkey_vs_int(lhs, rhs).or_else(|| pubkey_vs_int(rhs, lhs)) {
        anyhow::bail!(
            "handler `{}` {} compares Pubkey `{}` with numeric literal `{}`. \
             The DSL has no Pubkey-literal syntax — compare against a handler parameter, \
             a constant, or the spec's `program_id` instead.",
            handler_name,
            clause_kind,
            path_str,
            v
        );
    }
    Ok(())
}

fn numeric_literal_value(
    expr: &Expr,
    const_literals: &std::collections::HashMap<String, i128>,
) -> Option<i128> {
    match expr {
        // Integer literals stay non-negative at the AST (`Expr::Int(u128)`);
        // negative literals desugar to `Arith { Sub, Int(0), Int(v) }` and
        // are recognized via the Sub branch below.
        Expr::Int(v) => i128::try_from(*v).ok(),
        Expr::Path(p) if p.segments.is_empty() => const_literals.get(&p.root).copied(),
        Expr::Paren(inner) | Expr::Old(inner) => numeric_literal_value(&inner.node, const_literals),
        Expr::Arith {
            op: a::ArithOp::Sub,
            lhs,
            rhs,
        } => {
            let l = numeric_literal_value(&lhs.node, const_literals)?;
            let r = numeric_literal_value(&rhs.node, const_literals)?;
            l.checked_sub(r)
        }
        _ => None,
    }
}

fn render_path_human(p: &a::Path) -> String {
    let mut out = p.root.clone();
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

/// Translate the typed AST into a `ParsedSpec` compatible with current consumers.
pub fn adapt(spec: &a::Spec) -> ParsedSpec {
    let mut out = ParsedSpec {
        program_name: spec.name.clone(),
        ..ParsedSpec::default()
    };

    // First pass: collect constants so guard rendering can substitute them.
    let mut consts_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // Integer-max builtins so `requires state.x + n <= U64_MAX` resolves
    // out of the box. User-defined `const` shadows the builtin (builtins
    // inserted first, user consts after).
    consts_map.insert("U64_MAX".to_string(), u64::MAX.to_string());
    consts_map.insert("U32_MAX".to_string(), u32::MAX.to_string());
    consts_map.insert("U128_MAX".to_string(), u128::MAX.to_string());
    consts_map.insert("I64_MAX".to_string(), i64::MAX.to_string());
    consts_map.insert("I128_MAX".to_string(), i128::MAX.to_string());
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
                    // Flatten variant fields into the state-field list,
                    // deduped by name — duplicate fields would emit invalid
                    // Rust in the Kani harness. First occurrence wins
                    // (variants usually share field shapes); same-name
                    // different-type collisions surface via the check.rs lint.
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
                    // Preserve per-variant structure alongside the flattened
                    // `fields` view: enum-emitting codegen reads `variants`;
                    // the flat view stays for back-compat. Zero-payload
                    // variants (`| Inactive`) are kept for unit-style enum
                    // emission.
                    let parsed_variants: Vec<ParsedVariant> = adt
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
                    out.account_types.push(ParsedAccountType {
                        name: adt.name.clone(),
                        fields,
                        lifecycle,
                        pda_ref: None,
                        variants: parsed_variants,
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
                // Pick state_mode / Lean ctx by the property's class. Unary
                // renders `s.x`; Binary (body contains `old(...)`) renders
                // `post.x` / `pre.x` on the Rust side and `s'.x` / `s.x`
                // (`Ctx::Ensures`) on the Lean side — rendering both classes
                // identically would collapse every `old(...)` preservation
                // property into a structural tautology (`s.x ≥ s.x`).
                let property_class = classify_property_body(&p.body);
                let lean_ctx = match property_class {
                    crate::check::PropertyClass::Unary => Ctx::Guard,
                    crate::check::PropertyClass::Binary => Ctx::Ensures,
                };
                let lean = expr_to_lean(&p.body.node, lean_ctx, consts, &env);
                let property_state_mode = match property_class {
                    crate::check::PropertyClass::Unary => StateMode::Unary,
                    crate::check::PropertyClass::Binary => StateMode::Binary,
                };
                let native_opts = opts_native(&env).with_state_mode(property_state_mode);
                let pod_opts = opts_pod(&env).with_state_mode(property_state_mode);
                let rust = expr_to_rust(&p.body.node, Ctx::Guard, consts, native_opts);
                let rust_pod = expr_to_rust(&p.body.node, Ctx::Guard, consts, pod_opts);
                let preserved = match &p.preserved_by {
                    // `preserved_by all` — kept as the sentinel "all".
                    // Expanded to the full handler-name list below after all
                    // handlers are known (matches pest parity).
                    a::PreservedBy::All => vec!["all".to_string()],
                    a::PreservedBy::Some(xs) => xs.clone(),
                    // `all except [...]`: tag each name with a `!` prefix so
                    // the second-pass expansion (after all handlers are
                    // known) can subtract them. `!` isn't legal in
                    // identifiers, so the tag is collision-free.
                    a::PreservedBy::AllExcept(xs) => {
                        let mut tagged = vec!["all".to_string()];
                        for x in xs {
                            tagged.push(format!("!{}", x));
                        }
                        tagged
                    }
                };
                // When the body is a single `forall <binder> : <T>, inner`
                // with a binder type wider than U8/I8 (so the standard
                // proptest lowering would emit the unsupported sentinel),
                // also render the inner body keeping the binder as a free
                // Rust variable. proptest_gen uses this to emit per-slot
                // `_at` predicates and have preservation tests for handlers
                // taking the binder as a param check at that slot.
                let per_slot = match &p.body.node {
                    Expr::Quant {
                        kind: a::Quantifier::Forall,
                        binder,
                        binder_ty,
                        body,
                    } if !matches!(binder_ty.as_str(), "U8" | "I8") => {
                        let body_rust =
                            expr_to_rust(&body.node, Ctx::Guard, consts, opts_native(&env));
                        // Only useful if the body itself rendered without any
                        // further unsupported quantifier — nested wide forall
                        // can't be flattened to a single per-slot param.
                        if !crate::check::rust_expr_is_unsupported(&body_rust) {
                            Some(crate::check::PerSlotForm {
                                binder_name: binder.clone(),
                                binder_type: binder_ty.clone(),
                                rust_body: body_rust,
                            })
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                // Classify the quantifier shape so `check_completeness` can
                // lint unsupported shapes. Supported shapes get `None`; the
                // per_slot field above carries what codegen needs.
                let quantifier_lint = match crate::quantifier::supported_shape(p) {
                    Ok(_) => None,
                    // A bounded `exists` (over `Fin[N]`, directly or via an
                    // alias such as `AccountIdx`) now lowers to a real
                    // `(0..N).any(…)` harness predicate — see the `expr_to_rust`
                    // quantifier arm. `supported_shape` rejects every `exists`
                    // because it can't resolve aliases to decide boundedness;
                    // the rendered Rust body is the ground truth, so when the
                    // body lowered cleanly (no `QEDGEN_UNSUPPORTED` sentinel)
                    // suppress the spurious P5. Unbounded `exists` (e.g. over
                    // `U64`) still emits the sentinel and keeps the lint.
                    Err(crate::quantifier::Reason::ExistsQuantifier { .. })
                        if !crate::check::rust_expr_is_unsupported(&rust) =>
                    {
                        None
                    }
                    Err(reason) => {
                        let kind = match &reason {
                            crate::quantifier::Reason::NestedQuantifier { .. } => {
                                "nested_quantifier"
                            }
                            crate::quantifier::Reason::UnboundedBinderType { .. } => {
                                "unbounded_binder"
                            }
                            crate::quantifier::Reason::ExistsQuantifier { .. } => {
                                "exists_quantifier"
                            }
                        };
                        let span = reason.span();
                        Some(crate::check::QuantifierLint {
                            kind: kind.to_string(),
                            message: reason.message(),
                            span_start: span.start,
                            span_end: span.end,
                        })
                    }
                };
                let class = classify_property_body(&p.body);
                out.properties.push(ParsedProperty {
                    name: p.name.clone(),
                    expression: Some(lean),
                    rust_expression: Some(rust),
                    rust_expression_pod: Some(rust_pod),
                    preserved_by: preserved,
                    per_slot,
                    quantifier_lint,
                    class,
                    ast_body: Some(p.body.clone()),
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
                let parsed = match &i.body {
                    a::InvariantBody::Expr(e) => {
                        let lean = expr_to_lean(&e.node, Ctx::Guard, consts, &env);
                        let rust =
                            crate::rust_codegen_util::translate_property_to_rust(&lean, false);
                        crate::check::ParsedInvariant {
                            name: i.name.clone(),
                            doc: String::new(),
                            lean_expr: Some(lean),
                            rust_expr: Some(rust),
                            ast_body: Some(e.clone()),
                        }
                    }
                    a::InvariantBody::Description(s) => crate::check::ParsedInvariant {
                        name: i.name.clone(),
                        doc: s.clone(),
                        lean_expr: None,
                        rust_expr: None,
                        ast_body: None,
                    },
                };
                out.invariants.push(parsed);
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
                            constraints_rust.push(expr_to_rust(
                                &e.node,
                                Ctx::Ensures,
                                consts,
                                opts_native(&env),
                            ));
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
            TopItem::Import {
                name,
                from,
                as_name,
            } => {
                out.imports.push(ParsedImport {
                    name: name.clone(),
                    from: from.clone(),
                    as_name: as_name.clone(),
                });
            }
            TopItem::PragmaAssign { name, value } => {
                // Push raw; lint validates the key against the known set and
                // the value against declared `type Error` variants.
                out.pragma_assignments.push((name.clone(), value.clone()));
            }
            TopItem::Schema(s) => {
                // Collect schema blocks for `include <name>` expansion;
                // adapt each requires eagerly so schema guards land in the
                // same shape as inline requires.
                let requires = s
                    .requires
                    .iter()
                    .map(|(guard, on_fail)| ParsedRequires {
                        lean_expr: expr_to_lean(&guard.node, Ctx::Guard, consts, &env),
                        rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_native(&env)),
                        rust_expr_pod: expr_to_rust(
                            &guard.node,
                            Ctx::Guard,
                            consts,
                            opts_pod(&env),
                        ),
                        error_name: on_fail.clone(),
                        ast_body: Some(guard.clone()),
                    })
                    .collect();
                out.schemas.push(crate::check::ParsedSchema {
                    name: s.name.clone(),
                    doc: s.doc.clone(),
                    requires,
                });
            }
            TopItem::RefImpl(r) => {
                // Lean lowering uses `lean_body`; Kani inlining uses
                // `rust_body`. The body is pure — use Guard ctx so bare
                // state refs render as `s.x` (single-state context).
                let params: Vec<(String, String)> = r
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                    .collect();
                let lean_body = expr_to_lean(&r.body.node, Ctx::Guard, consts, &env);
                let rust_body = expr_to_rust(&r.body.node, Ctx::Guard, consts, opts_native(&env));
                out.ref_impls.push(crate::check::ParsedRefImpl {
                    name: r.name.clone(),
                    doc: r.doc.clone(),
                    params,
                    return_type: type_ref_to_string(&r.return_type),
                    lean_body,
                    rust_body,
                });
            }
            TopItem::Ghost(g) => {
                // Init value — a constant expression in single-state context.
                let init_lean = expr_to_lean(&g.init.node, Ctx::Guard, consts, &env);
                let init_rust = expr_to_rust(&g.init.node, Ctx::Guard, consts, opts_native(&env));
                let mut updates = Vec::new();
                for u in &g.updates {
                    // Resolve the named handler's params so the update RHS can
                    // reference them; ghost names already resolve via `env`.
                    let handler_params: &[a::TypedField] = spec
                        .items
                        .iter()
                        .find_map(|it| match &it.node {
                            TopItem::Handler(h) if h.name == u.handler => Some(h.params.as_slice()),
                            _ => None,
                        })
                        .unwrap_or(&[]);
                    let uenv = env.clone().with_params(handler_params);
                    let rhs_lean = expr_to_lean(&u.stmt.rhs.node, Ctx::Guard, consts, &uenv);
                    let rhs_rust =
                        expr_to_rust(&u.stmt.rhs.node, Ctx::Guard, consts, opts_native(&uenv));
                    // Fold the assignment operator into a complete new-value
                    // expression so each backend just emits `<ghost> := value`.
                    let (value_lean, value_rust) = match u.stmt.op {
                        a::EffectOp::Set => (rhs_lean, rhs_rust),
                        a::EffectOp::Add | a::EffectOp::AddSat | a::EffectOp::AddWrap => (
                            format!("s.{} + ({})", g.name, rhs_lean),
                            format!("s.{} + ({})", g.name, rhs_rust),
                        ),
                        a::EffectOp::Sub | a::EffectOp::SubSat | a::EffectOp::SubWrap => (
                            format!("s.{} - ({})", g.name, rhs_lean),
                            format!("s.{} - ({})", g.name, rhs_rust),
                        ),
                    };
                    updates.push(crate::check::ParsedGhostUpdate {
                        handler: u.handler.clone(),
                        value_lean,
                        value_rust,
                    });
                }
                out.ghosts.push(crate::check::ParsedGhost {
                    name: g.name.clone(),
                    doc: g.doc.clone(),
                    ty: type_ref_to_string(&g.ty),
                    init_lean,
                    init_rust,
                    updates,
                });
            }
            TopItem::Hook(h) => {
                let kind = match &h.kind {
                    a::HookKind::AfterStore(field) => {
                        crate::check::ParsedHookKind::AfterStore(field.clone())
                    }
                    a::HookKind::BeforeCpi(iface) => {
                        crate::check::ParsedHookKind::BeforeCpi(iface.clone())
                    }
                };
                let asserts = h
                    .asserts
                    .iter()
                    .map(|e| crate::check::ParsedHookAssert {
                        lean: expr_to_lean(&e.node, Ctx::Guard, consts, &env),
                        rust: expr_to_rust(&e.node, Ctx::Guard, consts, opts_native(&env)),
                    })
                    .collect();
                out.hooks.push(crate::check::ParsedHook { kind, asserts });
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

    // Expand `preserved_by all` to the full handler-name list. `all except
    // [...]` arrives as `["all", "!h1", "!h2"]`; expand `all` then subtract
    // the `!`-prefixed excludes.
    let all_handler_names: Vec<String> = out.handlers.iter().map(|h| h.name.clone()).collect();
    for prop in &mut out.properties {
        if prop.preserved_by.first().map(|s| s.as_str()) == Some("all") {
            let excludes: std::collections::HashSet<String> = prop
                .preserved_by
                .iter()
                .filter_map(|s| s.strip_prefix('!').map(String::from))
                .collect();
            if excludes.is_empty() && prop.preserved_by.len() == 1 {
                prop.preserved_by = all_handler_names.clone();
            } else {
                prop.preserved_by = all_handler_names
                    .iter()
                    .filter(|n| !excludes.contains(n.as_str()))
                    .cloned()
                    .collect();
            }
            continue;
        }
        if prop.preserved_by.len() == 1 && prop.preserved_by[0] == "all" {
            prop.preserved_by = all_handler_names.clone();
        }
    }

    // Promote the `state { ... }` sugar's record into `account_types` so
    // lints that walk `account_types` (notably `check_map_and_subscript`)
    // see Map-typed fields — otherwise `lsts[idx].x := …` fires a spurious
    // `subscript_not_map`. Only synthesize when there's no explicit `type
    // State` ADT; the State record stays available via `out.records`.
    if out.account_types.is_empty() {
        if let Some(state_record) = out.records.iter().find(|r| r.name == "State") {
            out.account_types.push(ParsedAccountType {
                name: "State".to_string(),
                fields: state_record.fields.clone(),
                lifecycle: Vec::new(),
                pda_ref: None,
                variants: Vec::new(),
            });
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

    // When the spec uses `state { ... }` sugar or `type State = { ... }`
    // and a handler has no explicit `accounts { ... }`, synthesize a
    // default state-bearing account so codegen can bind `state.X` refs —
    // otherwise guards.rs emits raw `s.X` against no account. Gated on
    // `records.contains("State")` so ADT-state specs stay unchanged. The
    // synthetic name "state" matches lowercase "State" so
    // `infer_state_name`'s case-insensitive lookup binds it to the
    // canonical `<ProgramPascalCase>Account` struct.
    let is_state_record_form = out.records.iter().any(|r| r.name == "State");
    if is_state_record_form && !out.state_fields.is_empty() {
        // `path_to_lean` already lowered `state.X` to `s.X` (Ctx::Guard)
        // or `s'.X` (Ctx::Ensures) before storing in lean_expr, so the
        // textual marker we look for is the lowered prefix, not the
        // spec-level `state.` form. Word-bound the scan so identifiers
        // like `is_active` or method names ending in `s` don't trip it.
        let mentions_state = |text: &str| {
            let bytes = text.as_bytes();
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i] != b's' {
                    continue;
                }
                let prev_word_char =
                    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                if prev_word_char {
                    continue;
                }
                let next = bytes[i + 1];
                if next == b'.' {
                    return true;
                }
                if next == b'\'' && i + 2 < bytes.len() && bytes[i + 2] == b'.' {
                    return true;
                }
            }
            false
        };
        for handler in &mut out.handlers {
            if !handler.accounts.is_empty() {
                continue;
            }
            let touches_state = !handler.effects.is_empty()
                || handler
                    .requires
                    .iter()
                    .any(|r| mentions_state(&r.lean_expr))
                || handler
                    .aborts_if
                    .iter()
                    .any(|a| mentions_state(&a.lean_expr))
                || handler.ensures.iter().any(|e| mentions_state(&e.lean_expr));
            if !touches_state {
                continue;
            }
            handler.accounts.push(ParsedHandlerAccount {
                name: "state".to_string(),
                is_signer: false,
                is_writable: !handler.effects.is_empty(),
                is_program: false,
                pda_seeds: None,
                account_type: Some("State".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            });
        }
    }

    out.constants = constants;
    // Collect uninterpreted helpers after all other fields are populated —
    // the collector needs the full state_fields + records + sum_types
    // picture to infer argument types. Names declared as `ref_impl`s are
    // filtered out: they have real Lean bodies (`def`), so the
    // uninterpreted `opaque foo : T → Bool` shape would conflict.
    out.uninterpreted_helpers = collect_uninterpreted_helpers(spec, &out);
    let ref_impl_names: std::collections::HashSet<&str> =
        out.ref_impls.iter().map(|r| r.name.as_str()).collect();
    out.uninterpreted_helpers
        .retain(|(n, _, _)| !ref_impl_names.contains(n.as_str()));

    // Expand `include <schema>` clauses as a post-pass so the lookup sees
    // every declared schema regardless of source order, and synthetic
    // match-arm handlers (which inherit `schema_includes`) get the same
    // expansion.
    let schemas = out.schemas.clone();
    for handler in &mut out.handlers {
        if handler.schema_includes.is_empty() {
            continue;
        }
        for include in &handler.schema_includes {
            if let Some(schema) = schemas.iter().find(|s| s.name == *include) {
                for r in &schema.requires {
                    handler.requires.push(r.clone());
                }
            }
        }
    }

    // Desugar dotted-auth (`auth admin_config.admin_key`): split on `.`,
    // identify the signer, synthesize `requires <acct>.<field> ==
    // <signer>.pubkey else Unauthorized`, and rewrite `handler.who` to the
    // signer so "handler has auth" lints still fire. Bare `auth <name>`
    // stays untouched (lowers to Anchor `has_one = X`).
    desugar_dotted_auth(&mut out);

    out
}

/// Post-pass: `auth <acct>.<field>` → synthesized `requires <acct>.<field>
/// == <signer>.pubkey else Unauthorized` + `who = <signer>` rewrite.
fn desugar_dotted_auth(spec: &mut ParsedSpec) {
    for handler in &mut spec.handlers {
        let Some(actor) = handler.who.clone() else {
            continue;
        };
        let Some((acct_name, field)) = actor.split_once('.') else {
            continue;
        };
        // Identify the signer (single signer in accounts block).
        // Multiple signers → bail without desugaring; the user should
        // explicitly write the requires clause.
        let signers: Vec<&str> = handler
            .accounts
            .iter()
            .filter(|a| a.is_signer)
            .map(|a| a.name.as_str())
            .collect();
        if signers.len() != 1 {
            continue;
        }
        let signer = signers[0].to_string();
        // Synthesize the requires clause. Lean form uses `=` (the
        // adapter's Lean output convention); Rust form uses `==` per
        // `expr_to_rust(Ctx::Guard)`. Both forms read the imported
        // account through the handler-account binding name, which
        // `bind_state` in guards.rs rewrites to `ctx.<acct>.<field>`
        // (flat-struct path) or `(*ctx.<acct>.inner.<field>())`
        // (multi-variant ADT) at codegen time.
        let lean_expr = format!("{}.{} = {}.pubkey", acct_name, field, signer);
        let rust_expr = format!("{}.{} == {}.pubkey", acct_name, field, signer);
        let synthesized = crate::check::ParsedRequires {
            lean_expr,
            rust_expr: rust_expr.clone(),
            rust_expr_pod: rust_expr,
            error_name: Some("Unauthorized".to_string()),
            ast_body: None,
        };
        handler.requires.insert(0, synthesized);
        // Rewrite `who` to the signer so downstream consumers see a
        // simple-ident auth (no_access_control lint, lifecycle gates,
        // etc.).
        handler.who = Some(signer);
    }
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
        aliases: base_env.aliases.clone(),
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
    // later arms' precondition (first-match semantics). Triple is
    // (lean, rust_native, rust_pod).
    let mut prior_conds: Vec<(String, String, String)> = Vec::new();
    let mut out = Vec::with_capacity(branch.arms.len());

    for arm in &branch.arms {
        let mut synth = base.clone();
        synth.name = format!("{}_{}", h.name, arm.label);

        // Add all prior-arm negations to this arm's requires.
        for (lean_neg, rust_neg, rust_pod_neg) in &prior_conds {
            synth.requires.push(ParsedRequires {
                lean_expr: lean_neg.clone(),
                rust_expr: rust_neg.clone(),
                rust_expr_pod: rust_pod_neg.clone(),
                error_name: None,
                // Synthetic (prior arms' negations, no single source AST
                // node); the `old_in_single_state_context` lint skips these.
                ast_body: None,
            });
        }

        // Current arm's guard (if any) becomes a requires; negation is
        // recorded for subsequent arms.
        if let Some(guard) = &arm.guard {
            let lean = expr_to_lean(&guard.node, Ctx::Guard, consts, env);
            let rust = expr_to_rust(&guard.node, Ctx::Guard, consts, opts_native(env));
            let rust_pod = expr_to_rust(&guard.node, Ctx::Guard, consts, opts_pod(env));
            synth.requires.push(ParsedRequires {
                lean_expr: lean.clone(),
                rust_expr: rust.clone(),
                rust_expr_pod: rust_pod.clone(),
                error_name: None,
                ast_body: Some(guard.clone()),
            });
            prior_conds.push((
                format!("\u{00AC}({})", lean),
                format!("!({})", rust),
                format!("!({})", rust_pod),
            ));
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
                    rust_expr_pod: "false".to_string(),
                    error_name: Some(err.clone()),
                    // Synthetic: arm-abort lowers to literal `false` with no
                    // source AST; lint skips.
                    ast_body: None,
                });
            }
            a::MatchBody::Effect(stmts) => {
                for Node { node: stmt, .. } in stmts {
                    for (triple, on_error) in
                        render_effect_or_expand_variant_promotion(stmt, &base.takes_params, consts)
                    {
                        synth.effects.push(triple);
                        synth.effect_on_error.push(on_error);
                    }
                }
            }
            // Synth handler issues the CPI plus any alongside effects —
            // mirrors the top-level `Call` clause lowering so backends see
            // the same ParsedCall shape either way.
            a::MatchBody::Call(call, effects) => {
                let segs = &call.target.0;
                let (iface, handler_name) = match segs.as_slice() {
                    [] => (String::new(), String::new()),
                    [only] => (String::new(), only.clone()),
                    [head, tail @ ..] => (head.clone(), tail.join(".")),
                };
                let args = call
                    .args
                    .iter()
                    .map(|arg| ParsedCallArg {
                        name: arg.name.clone(),
                        lean_expr: expr_to_lean(&arg.value.node, Ctx::Guard, consts, env),
                        rust_expr: expr_to_rust(
                            &arg.value.node,
                            Ctx::Guard,
                            consts,
                            opts_native(env),
                        ),
                        rust_expr_pod: expr_to_rust(
                            &arg.value.node,
                            Ctx::Guard,
                            consts,
                            opts_pod(env),
                        ),
                    })
                    .collect();
                synth.calls.push(ParsedCall {
                    target_interface: iface,
                    target_handler: handler_name,
                    args,
                    result_binding: call.result_binding.clone(),
                    // Match-arm CPIs always carry an empty binder list at
                    // the parser; lower_state_binders still runs so the
                    // codepath stays uniform.
                    state_binders: lower_state_binders(&call.state_binders),
                });
                for Node { node: stmt, .. } in effects {
                    for (triple, on_error) in
                        render_effect_or_expand_variant_promotion(stmt, &base.takes_params, consts)
                    {
                        synth.effects.push(triple);
                        synth.effect_on_error.push(on_error);
                    }
                }
            }
            a::MatchBody::Noop => {}
        }

        out.push(synth);
    }

    out
}

/// Lower `state_binders { callee_field = state.X, ... }` into the
/// `ParsedStateBinder` shape backends thread to the Lean axiom signature
/// and Kani harness substitution. Each RHS must be exactly
/// `state.<ident>`; richer paths drop the binder with a stderr warning —
/// the spec lint catches the same shape pre-codegen, so this is a
/// defensive guard. Duplicate callee_fields keep the first occurrence
/// (dedup runs across coalesced blocks).
fn lower_state_binders(binders: &[a::StateBinder]) -> Vec<ParsedStateBinder> {
    let mut out: Vec<ParsedStateBinder> = Vec::new();
    for b in binders {
        // RHS must be `state.<ident>` — a `Path { root: "state",
        // segments: [Field(ident)] }`, possibly `Paren`-wrapped.
        let caller_field = extract_state_field(&b.caller_expr.node);
        match caller_field {
            Some(field) => {
                if !out.iter().any(|p| p.callee_field == b.callee_field) {
                    out.push(ParsedStateBinder {
                        callee_field: b.callee_field.clone(),
                        caller_field: field,
                    });
                }
            }
            None => {
                eprintln!(
                    "warning: state_binders RHS for `{}` must be `state.<field>` \
                     (v2.27 Track A restriction); binder dropped",
                    b.callee_field,
                );
            }
        }
    }
    out
}

/// Extract the trailing field ident from a `state.<ident>` binder RHS
/// (optional `Paren` wrappers); `None` if the shape doesn't match.
fn extract_state_field(e: &Expr) -> Option<String> {
    match e {
        Expr::Paren(inner) => extract_state_field(&inner.node),
        Expr::Path(p) => {
            if p.root == "state" && p.segments.len() == 1 {
                if let a::PathSeg::Field(f) = &p.segments[0] {
                    return Some(f.clone());
                }
            }
            None
        }
        _ => None,
    }
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
        effect_on_error: Vec::new(),
        accounts: Vec::new(),
        transfers: Vec::new(),
        emits: Vec::new(),
        invariants: Vec::new(),
        establishes: Vec::new(),
        schema_includes: Vec::new(),
        properties: Vec::new(),
        calls: Vec::new(),
        effect_branches: None,
        abstract_binders: Vec::new(),
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
                        default_pubkey: None,
                        imported_namespace: None,
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
                            a::AccountAttr::Type(t) => {
                                // Dotted type ref (`Foreign.State`) splits
                                // into namespace alias + source type; bare
                                // types keep `imported_namespace = None`.
                                // `imported_namespaces` is populated BEFORE
                                // handler adapt, so check.rs validates the
                                // namespace is known.
                                if let Some((ns, ty)) = t.split_once('.') {
                                    acc.imported_namespace = Some(ns.to_string());
                                    acc.account_type = Some(ty.to_string());
                                } else {
                                    acc.account_type = Some(t.clone());
                                }
                            }
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
                    rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_native(env)),
                    rust_expr_pod: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_pod(env)),
                    error_name: on_fail.clone(),
                    ast_body: Some(guard.clone()),
                });
            }
            a::HandlerClause::Ensures(e) => {
                handler.ensures.push(ParsedEnsures {
                    lean_expr: expr_to_lean(&e.node, Ctx::Ensures, consts, env),
                    rust_expr: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_native(env)),
                    rust_expr_pod: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_pod(env)),
                    // Binary rendering for the Kani ensures-preservation
                    // harness: `state.x` → `post.x`; `old(state.x)` → `pre.x`.
                    rust_expr_binary: expr_to_rust(
                        &e.node,
                        Ctx::Ensures,
                        consts,
                        opts_native(env).with_state_mode(StateMode::Binary),
                    ),
                });
            }
            a::HandlerClause::Modifies(fs) => {
                handler.modifies = Some(fs.clone());
            }
            a::HandlerClause::Let { name, value } => {
                let rust = expr_to_rust(&value.node, Ctx::Guard, consts, opts_native(env));
                // Narrow `let X = mul_div_*(...)` to U64 at the binding
                // site — the spec-level operation is U64 → U64 but the
                // u128 helper is used for intermediate `a * b` width.
                // Without this, the canonical `let fee =
                // mul_div_floor(total, fee_bps, BPS); let to_provider =
                // total - fee` lowers to `u64 - u128`, which `cargo
                // build` rejects. Wider arbitrary-expression contexts
                // (`requires X mul_div_floor(...) <= U128_LIT …`) keep
                // the u128 width via the `expr_to_rust` rendering above.
                //
                // `is_mul_div_let_rhs` peels `Paren` / `Old` wrappers so
                // `let X = (mul_div_floor(...))` and `let X =
                // old(mul_div_floor(...))` get the same narrowing as the
                // bare form — matches the precedent in `rust_infer_kind`.
                let rust = if is_mul_div_let_rhs(&value.node) {
                    format!("({}) as u64", rust)
                } else {
                    rust
                };
                handler.let_bindings.push((
                    name.clone(),
                    expr_to_lean(&value.node, Ctx::Guard, consts, env),
                    rust,
                ));
            }
            a::HandlerClause::Effect(blocks) => {
                // `effect { … }` may contain a top-level `match` block
                // alongside leaf statements. Two outputs:
                //   1. `handler.effects` — flat union of all leaves.
                //   2. `handler.effect_branches` — `Some` iff the spec
                //      uses `match`. Carries arm structure for branched
                //      emission in the Rust/Kani/proptest backends.
                let mut branches: Option<crate::check::ParsedEffectBranches> = None;
                for Node { node: block, .. } in blocks {
                    match block {
                        a::EffectBlock::Stmt(stmt) => {
                            for (triple, on_error) in
                                render_effect_or_expand_variant_promotion(stmt, &params, consts)
                            {
                                handler.effects.push(triple);
                                handler.effect_on_error.push(on_error);
                            }
                        }
                        a::EffectBlock::Match { scrutinee, arms } => {
                            let mut parsed_arms: Vec<crate::check::ParsedEffectArm> = Vec::new();
                            for arm in arms {
                                let mut arm_effects = Vec::new();
                                let mut arm_on_error: Vec<Option<String>> = Vec::new();
                                for nested in &arm.body {
                                    let mut leaves = Vec::new();
                                    nested.node.collect_leaves(&mut leaves);
                                    for stmt in leaves {
                                        for (triple, on_error) in
                                            render_effect_or_expand_variant_promotion(
                                                stmt, &params, consts,
                                            )
                                        {
                                            // Mirror into union so flat
                                            // readers see this potential
                                            // write.
                                            handler.effects.push(triple.clone());
                                            handler.effect_on_error.push(on_error.clone());
                                            arm_effects.push(triple);
                                            arm_on_error.push(on_error);
                                        }
                                    }
                                }
                                let (pattern_rust, pattern_lean, is_wildcard) = match &arm.pattern {
                                    a::EffectPattern::Literal(v) => {
                                        (v.to_string(), v.to_string(), false)
                                    }
                                    a::EffectPattern::Wildcard => {
                                        ("_".to_string(), "_".to_string(), true)
                                    }
                                };
                                parsed_arms.push(crate::check::ParsedEffectArm {
                                    pattern_rust,
                                    pattern_lean,
                                    is_wildcard,
                                    effects: arm_effects,
                                    effect_on_error: arm_on_error,
                                });
                            }
                            branches = Some(crate::check::ParsedEffectBranches {
                                scrutinee_rust: expr_to_rust(
                                    &scrutinee.node,
                                    Ctx::Guard,
                                    consts,
                                    opts_native(env),
                                ),
                                scrutinee_rust_pod: expr_to_rust(
                                    &scrutinee.node,
                                    Ctx::Guard,
                                    consts,
                                    opts_pod(env),
                                ),
                                scrutinee_lean: expr_to_lean(
                                    &scrutinee.node,
                                    Ctx::Guard,
                                    consts,
                                    env,
                                ),
                                arms: parsed_arms,
                            });
                        }
                    }
                }
                if branches.is_some() {
                    handler.effect_branches = branches;
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
            a::HandlerClause::Establishes(name) => handler.establishes.push(name.clone()),
            a::HandlerClause::Abstract { name, ty } => {
                // Stored as (name, DSL-type-string); each backend maps the
                // DSL type to its own representation.
                handler
                    .abstract_binders
                    .push((name.clone(), type_ref_to_string(ty)));
            }
            a::HandlerClause::Include(schema_name) => {
                // Record the reference only; expansion happens in the
                // post-pass at the bottom of `adapt()` once every schema is
                // known. Storing the list on the handler lets it survive the
                // synthetic-match-arm expansion (each arm inherits it).
                handler.schema_includes.push(schema_name.clone());
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
                        rust_expr: expr_to_rust(
                            &arg.value.node,
                            Ctx::Guard,
                            consts,
                            opts_native(env),
                        ),
                        rust_expr_pod: expr_to_rust(
                            &arg.value.node,
                            Ctx::Guard,
                            consts,
                            opts_pod(env),
                        ),
                    })
                    .collect();
                handler.calls.push(ParsedCall {
                    target_interface: iface,
                    target_handler: handler_name,
                    args,
                    result_binding: c.result_binding.clone(),
                    // Empty when no binders declared; backends keep the
                    // callee-frame, param-only axiom in that case.
                    state_binders: lower_state_binders(&c.state_binders),
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
        // Interface-level `state { ... }` block as (name, type-string)
        // pairs; empty when not declared.
        state_fields: iface
            .state_fields
            .iter()
            .map(|f| (f.name.clone(), type_ref_to_string(&f.ty)))
            .collect(),
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
        return_type: h.return_type.as_ref().map(type_ref_to_string),
        // `None` = no binder written (bare `-> Type` or nothing);
        // downstream substitution defaults to the literal `"result"`.
        result_binder: h.result_binder.clone(),
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
                        default_pubkey: None,
                        imported_namespace: None,
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
                            a::AccountAttr::Type(t) => {
                                if let Some((ns, ty)) = t.split_once('.') {
                                    acc.imported_namespace = Some(ns.to_string());
                                    acc.account_type = Some(ty.to_string());
                                } else {
                                    acc.account_type = Some(t.clone());
                                }
                            }
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
                    rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_native(env)),
                    rust_expr_pod: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_pod(env)),
                    error_name: on_fail.clone(),
                    ast_body: Some(guard.clone()),
                });
            }
            a::InterfaceHandlerClause::Ensures(e) => {
                out.ensures.push(ParsedEnsures {
                    lean_expr: expr_to_lean(&e.node, Ctx::Ensures, consts, env),
                    rust_expr: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_native(env)),
                    rust_expr_pod: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_pod(env)),
                    rust_expr_binary: expr_to_rust(
                        &e.node,
                        Ctx::Ensures,
                        consts,
                        opts_native(env).with_state_mode(StateMode::Binary),
                    ),
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
        include_str!("../../../../examples/rust/percolator/percolator.qedspec");

    /// `Map[<EnumType>] T` is recognized when the bound names a unit-only
    /// enum (all variants payload-free).
    #[test]
    fn map_keyed_by_enum_routes_to_sum_types() {
        let src = r#"spec EnumMap
program_id "11111111111111111111111111111111"

type AddressField
  | Owner
  | Manager
  | Treasury

type ProposalSlot = { proposed : Pubkey, deadline : U64, }

type State
  | Active of {
      proposals : Map[AddressField] ProposalSlot,
    }

type Error
  | NoMatch
"#;
        let spec = parse_str(src).expect("parse");
        // AddressField should route to sum_types (not account_types)
        // because it's used as a Map key.
        let has_sum = spec.sum_types.iter().any(|s| s.name == "AddressField");
        assert!(
            has_sum,
            "AddressField should land in sum_types when used as a Map key; got sum_types: {:?}",
            spec.sum_types.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        // And the unit-only check passes (all variants payload-free).
        let af = spec
            .sum_types
            .iter()
            .find(|s| s.name == "AddressField")
            .unwrap();
        assert!(
            af.variants.iter().all(|v| v.fields.is_empty()),
            "AddressField should be unit-only"
        );
    }

    /// `state := .Variant { f := e, ... }` desugars into per-field effects
    /// with variant-prefixed LHS for `emit_cross_variant_promotion`.
    #[test]
    fn state_variant_promotion_expands_to_per_field_effects() {
        let src = r#"spec Lifecycle
program_id "11111111111111111111111111111111"

type State
  | Uninitialized
  | Setup of { admin : Pubkey, balance : U64, }

type Error
  | WrongState

handler initialize : State.Uninitialized -> State.Setup {
  accounts {
    admin : signer
  }
  effect {
    state := .Setup { admin := admin.pubkey, balance := 0 }
  }
}
"#;
        let spec = parse_str(src).expect("parse");
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "initialize")
            .expect("initialize handler");
        // The single `state := .Setup { ... }` effect should have
        // expanded into two per-field effects with variant-prefixed
        // LHS, not a single bare-state effect.
        let lhs_strs: Vec<&String> = handler.effects.iter().map(|(lhs, _, _)| lhs).collect();
        assert!(
            lhs_strs.iter().any(|s| s.as_str() == "Setup.admin"),
            "expected Setup.admin in effect LHS list; got: {:?}",
            lhs_strs
        );
        assert!(
            lhs_strs.iter().any(|s| s.as_str() == "Setup.balance"),
            "expected Setup.balance in effect LHS list; got: {:?}",
            lhs_strs
        );
        assert!(
            !lhs_strs.iter().any(|s| s.as_str() == "state"),
            "bare-state LHS should have been desugared away; got: {:?}",
            lhs_strs
        );
    }

    /// `abstract <name> : <Type>` clauses parse into
    /// `ParsedHandler::abstract_binders` as (name, verbatim DSL type).
    #[test]
    fn abstract_binder_clause_parses_into_handler() {
        let src = r#"spec Earn
program_id "11111111111111111111111111111111"

type State
  | Active of { lp_supply : U64 }

type Error
  | InvalidAmount

handler user_deposit (amount_stablecoin : U64) : State.Active -> State.Active {
  accounts { user : signer }
  abstract d : U64
  requires d > 0 else InvalidAmount
  requires d <= amount_stablecoin else InvalidAmount
  effect { lp_supply += d }
}
"#;
        let spec = parse_str(src).expect("parse");
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "user_deposit")
            .expect("user_deposit handler");
        assert_eq!(handler.abstract_binders.len(), 1);
        assert_eq!(handler.abstract_binders[0].0, "d");
        assert_eq!(handler.abstract_binders[0].1, "U64");
    }

    /// Negative integer literals desugar to `Arith { Sub, Int(0), Int(v) }`
    /// at parse time.
    #[test]
    fn negative_integer_literal_parses_as_sub_of_zero() {
        let src = r#"spec Exp
program_id "11111111111111111111111111111111"

type State
  | Active of { exp : U64 }

type Error
  | Bad

handler set_exp : State.Active -> State.Active {
  accounts { authority : signer }
  requires state.exp == -4 else Bad
  effect { exp := 0 }
}
"#;
        // The successful parse is the assertion.
        let _spec = parse_str(src).expect("negative literal must parse");
    }

    /// `const NAME = -VALUE` parses with the value stored as a signed `i128`.
    #[test]
    fn const_decl_accepts_negative_literal() {
        let src = r#"spec ExpConst
program_id "11111111111111111111111111111111"

const N6 = -6

type State
  | Active of { exp : U64 }

type Error
  | Bad
"#;
        let spec = parse_str(src).expect("parse");
        let n6 = spec
            .constants
            .iter()
            .find(|(n, _)| n == "N6")
            .expect("N6 const must exist");
        assert_eq!(n6.1, "-6");
    }

    /// Unit-variant promotion (`state := .Closed`) drops to zero effects;
    /// the wrapper assignment in `emit_cross_variant_promotion` handles the
    /// transition from `handler.post_status` directly.
    #[test]
    fn state_unit_variant_promotion_emits_no_effects() {
        let src = r#"spec Lifecycle
program_id "11111111111111111111111111111111"

type State
  | Open of { x : U64 }
  | Closed

type Error
  | WrongState

handler close : State.Open -> State.Closed {
  accounts {
    authority : signer
  }
  effect {
    state := .Closed
  }
}
"#;
        let spec = parse_str(src).expect("parse");
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "close")
            .expect("close handler");
        assert!(
            handler.effects.is_empty(),
            "unit-variant promotion should desugar to zero effects; got: {:?}",
            handler.effects
        );
        assert_eq!(handler.post_status.as_deref(), Some("Closed"));
    }

    /// `call Interface.handler(...)` is legal inside a match arm body; the
    /// call-arm synth gets the CPI on its `calls` slot (same shape as a
    /// top-level call clause).
    #[test]
    fn match_arm_accepts_call_body() {
        let src = r#"spec MatchCall
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb_loss (amount : U64) {
    accounts { vault : writable }
  }
}

type State
  | Active of { pnl : I64 }

type Error
  | NoMatch

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  match
    | state.pnl < 0 => call Pool.absorb_loss(amount = loss)
    | _ => abort NoMatch
}
"#;
        let spec = parse_str(src).expect("parse");
        // The match clause expands into one synth handler per arm.
        // The call-arm synth should have one ParsedCall on it.
        let synths: Vec<_> = spec
            .handlers
            .iter()
            .filter(|h| h.name.starts_with("liquidate"))
            .collect();
        let with_call: Vec<_> = synths.iter().filter(|h| !h.calls.is_empty()).collect();
        assert_eq!(
            with_call.len(),
            1,
            "expected exactly one synth handler with a call body; got {} synths total, {} with calls",
            synths.len(),
            with_call.len()
        );
        assert_eq!(with_call[0].calls[0].target_interface, "Pool");
        assert_eq!(with_call[0].calls[0].target_handler, "absorb_loss");
    }

    /// `let X = call Foo.handler(...)` records the binding name on
    /// `ParsedCall.result_binding`.
    #[test]
    fn call_with_let_binding_records_result_name() {
        let src = r#"spec CallLet
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb_loss (amount : U64) {
    accounts {
      vault : writable
    }
  }
}

type State
  | Active of { total_loss : U64 }

type Error
  | MathOverflow

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  let burned = call Pool.absorb_loss(amount = loss)
  effect { Active.total_loss += loss }
}

handler unbound_call : State.Active -> State.Active {
  permissionless
  call Pool.absorb_loss(amount = 1)
  effect { Active.total_loss += 1 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let liquidate = spec
            .handlers
            .iter()
            .find(|h| h.name == "liquidate")
            .expect("liquidate handler");
        let unbound = spec
            .handlers
            .iter()
            .find(|h| h.name == "unbound_call")
            .expect("unbound_call handler");
        assert_eq!(liquidate.calls.len(), 1);
        assert_eq!(
            liquidate.calls[0].result_binding.as_deref(),
            Some("burned"),
            "result_binding should carry the `let` name; got: {:?}",
            liquidate.calls[0].result_binding
        );
        assert_eq!(unbound.calls.len(), 1);
        assert_eq!(
            unbound.calls[0].result_binding, None,
            "bare `call …` keeps result_binding None"
        );
    }

    /// Top-level `schema name { requires … }` blocks parse, and `include
    /// <schema>` expands every schema requires into the handler's requires
    /// list at adapt time.
    #[test]
    fn schema_include_expands_into_handler_requires() {
        let src = r#"spec SchemaDemo
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, paused : U8 }

type Error
  | Paused
  | MathOverflow

schema gated_by_pause {
  requires state.paused == 0 else Paused
}

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  include gated_by_pause
  requires amount > 0 else MathOverflow
  effect { Active.balance += amount }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  permissionless
  include gated_by_pause
  requires amount > 0 else MathOverflow
  effect { Active.balance -= amount }
}
"#;
        let spec = parse_str(src).expect("parse");
        // Both handlers got the schema's `requires state.paused == 0`
        // appended to their existing `amount > 0` clause.
        for handler_name in ["deposit", "withdraw"] {
            let h = spec
                .handlers
                .iter()
                .find(|h| h.name == handler_name)
                .unwrap_or_else(|| panic!("missing handler {handler_name}"));
            assert!(
                h.requires.iter().any(|r| r.lean_expr.contains("paused")),
                "handler {handler_name} should pick up `paused` requires from gated_by_pause; got: {:?}",
                h.requires.iter().map(|r| &r.lean_expr).collect::<Vec<_>>()
            );
            assert!(
                h.requires
                    .iter()
                    .any(|r| r.error_name.as_deref() == Some("Paused")),
                "handler {handler_name} should pick up the schema's Paused error; got: {:?}",
                h.requires.iter().map(|r| &r.error_name).collect::<Vec<_>>()
            );
            assert!(
                h.schema_includes.contains(&"gated_by_pause".to_string()),
                "handler {handler_name} should remember its includes list"
            );
        }
        // Schema is also surfaced on spec.schemas for downstream
        // consumers (lint / docs / future tooling).
        assert!(
            spec.schemas.iter().any(|s| s.name == "gated_by_pause"),
            "spec.schemas should list gated_by_pause; got: {:?}",
            spec.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    /// `preserved_by all except [h1, h2]` expands to the full handler list
    /// minus the excluded names ("every handler other than the one whose
    /// job is to break it").
    #[test]
    fn preserved_by_all_except_expands_to_complement() {
        let src = r#"spec ExceptDemo
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, paused : U8 }

type Error
  | MathOverflow

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  requires amount > 0 else MathOverflow
  effect { Active.balance += amount }
}

handler pause : State.Active -> State.Active {
  permissionless
  effect { Active.paused := 1 }
}

handler unpause : State.Active -> State.Active {
  permissionless
  effect { Active.paused := 0 }
}

property still_unpaused :
  state.paused == 0
  preserved_by all except [pause]
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "still_unpaused")
            .expect("property still_unpaused");
        let names: std::collections::HashSet<&str> =
            prop.preserved_by.iter().map(String::as_str).collect();
        assert!(
            names.contains("deposit"),
            "expected deposit in preserved_by; got: {:?}",
            prop.preserved_by
        );
        assert!(
            names.contains("unpause"),
            "expected unpause in preserved_by; got: {:?}",
            prop.preserved_by
        );
        assert!(
            !names.contains("pause"),
            "pause should be excluded; got: {:?}",
            prop.preserved_by
        );
        assert!(
            !names.contains("all"),
            "sentinel `all` should be expanded away; got: {:?}",
            prop.preserved_by
        );
    }

    /// `invariant Foo` and `establishes Foo` route to distinct
    /// `ParsedHandler` fields. Backends key off the split: invariants →
    /// preserves (assume pre-state), establishes → no pre-assume.
    #[test]
    fn handler_invariant_clauses_route_to_invariants_vs_establishes() {
        let src = include_str!(
            "../../tests/fixtures/regressions/invariants/repro-establishes-clause.qedspec"
        );
        let spec = parse_str(src).expect("parse");
        let init = spec
            .handlers
            .iter()
            .find(|h| h.name == "init")
            .expect("init handler");
        let update = spec
            .handlers
            .iter()
            .find(|h| h.name == "update")
            .expect("update handler");
        assert_eq!(init.establishes, vec!["root_set".to_string()]);
        assert!(init.invariants.is_empty(), "init only `establishes`");
        assert_eq!(update.invariants, vec!["root_set".to_string()]);
        assert!(
            update.establishes.is_empty(),
            "update only `invariant` (preserves)"
        );
    }

    #[test]
    fn handler_invariant_clause_routes_to_invariants() {
        let src = include_str!(
            "../../tests/fixtures/regressions/invariants/repro-handler-invariant-clause.qedspec"
        );
        let spec = parse_str(src).expect("parse");
        for h in &spec.handlers {
            assert_eq!(
                h.invariants,
                vec!["count_bounded".to_string()],
                "handler {} should list count_bounded as `invariant`",
                h.name
            );
            assert!(h.establishes.is_empty());
        }
        // The top-level invariant decl carries the predicate body that the
        // adapter lowers via translate_property_to_rust.
        let inv = spec
            .invariants
            .iter()
            .find(|i| i.name == "count_bounded")
            .expect("count_bounded invariant decl");
        assert!(inv.lean_expr.is_some(), "lean_expr populated");
        assert!(inv.rust_expr.is_some(), "rust_expr populated");
        let rust = inv.rust_expr.as_deref().unwrap();
        assert!(
            rust.contains("s.count"),
            "rust_expr should reference s.count, got: {rust}"
        );
    }

    // Regression: Pubkey := <int> must be rejected at check time, not
    // deferred to lake build's "OfNat Pubkey 0" error.
    #[test]
    fn finding_7_pubkey_assign_from_int_rejected() {
        let src = include_str!(
            "../../tests/fixtures/regressions/issue-8/repro-07-pubkey-literal-assign.qedspec"
        );
        let err = parse_str(src).expect_err("expected Pubkey := 0 to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Pubkey field cannot be assigned a numeric literal"),
            "unexpected error message: {msg}"
        );
    }

    // Regression: state.<Pubkey> != <int> in a `requires` clause must also
    // be rejected.
    #[test]
    fn finding_8_pubkey_compare_with_int_rejected() {
        let src = include_str!(
            "../../tests/fixtures/regressions/issue-8/repro-08-pubkey-literal-compare.qedspec"
        );
        let err = parse_str(src).expect_err("expected state.key != 0 to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("compares Pubkey"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn finding_7_pubkey_assign_from_numeric_const_rejected() {
        let src = r#"spec Repro7Const
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h : State.Uninitialized -> State.Active {
  permissionless
  effect { key := ZERO }
}
"#;
        let err = parse_str(src).expect_err("expected key := ZERO to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Pubkey field cannot be assigned a numeric literal"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn finding_8_pubkey_compare_with_numeric_const_rejected() {
        let src = r#"spec Repro8Const
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h : State.Active -> State.Active {
  permissionless
  requires state.key != ZERO else E
  effect { }
}
"#;
        let err = parse_str(src).expect_err("expected state.key != ZERO to fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("compares Pubkey"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn pubkey_param_paths_remain_allowed_with_numeric_consts_present() {
        let src = r#"spec ReproConstGuard
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h (k : Pubkey) : State.Active -> State.Active {
  permissionless
  requires state.key != k else E
  effect { key := k }
}
"#;
        parse_str(src).expect("Pubkey param assignment/comparison should remain valid");
    }

    // Guard: bundled specs that legitimately compare/assign Pubkey
    // must not regress (e.g. `signer == state.admin`, `state.pk := p`).
    #[test]
    fn pubkey_typecheck_does_not_break_bundled_examples() {
        for src in [
            include_str!("../../../../examples/rust/escrow/escrow.qedspec"),
            include_str!("../../../../examples/rust/lending/lending.qedspec"),
            include_str!("../../../../examples/rust/multisig/multisig.qedspec"),
            include_str!("../../../../examples/rust/percolator/percolator.qedspec"),
            include_str!("../../tests/fixtures/regressions/issue-8/pool.qedspec"),
        ] {
            parse_str(src).unwrap();
        }
    }

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

    // Regression: property bodies referencing `state.x` must render as
    // `s.x` in the Rust form — `s` is the function parameter that
    // `emit_property_predicates` binds.
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
    fn property_forall_u8_lowers_to_iterator() {
        // U8 is small enough to exhaust (256 values) — must not emit the
        // unsupported sentinel; must lower to a `.all(|v| …)` expression.
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
            !crate::check::rust_expr_is_unsupported(rust),
            "U8 forall must lower to an iterator, not emit the unsupported marker: {}",
            rust
        );
        assert!(
            rust.contains("u8::MIN") && rust.contains("u8::MAX"),
            "must use u8 range: {}",
            rust
        );
        assert!(rust.contains(".all("), "must use .all(): {}", rust);
        assert!(
            !rust.contains('\u{2200}'),
            "rust must not contain ∀: {}",
            rust
        );
    }

    #[test]
    fn property_forall_large_type_marked_unsupported_in_rust() {
        // U64 cannot be exhausted in a test loop — must still emit the sentinel.
        let src = r#"spec T
state { x : U64 }
property forall_u64 :
  forall v : U64, v >= 0
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "forall_u64")
            .expect("property");
        let rust = prop.rust_expression.as_deref().expect("rust rendering");
        assert!(
            crate::check::rust_expr_is_unsupported(rust),
            "U64 forall must still emit the unsupported sentinel: {}",
            rust
        );
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

    // ----- adapter populates ParsedSpec.imports -----

    #[test]
    fn adapter_populates_imports() {
        let src = r#"spec T
import Token from "spl_token"
import MyAmm from "my_amm"
"#;
        let spec = parse_str(src).expect("parse");
        assert_eq!(spec.imports.len(), 2);
        assert_eq!(spec.imports[0].name, "Token");
        assert_eq!(spec.imports[0].from, "spl_token");
        assert_eq!(spec.imports[1].name, "MyAmm");
        assert_eq!(spec.imports[1].from, "my_amm");
    }

    #[test]
    fn adapter_imports_empty_for_specs_without_import_stmts() {
        let src = r#"spec T
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec.imports.is_empty());
    }

    // ----- if-then-else expressions -----

    #[test]
    fn if_then_else_renders_to_lean_native_form() {
        let src = r#"spec T
type State | A of { x : U64, y : U64 }
property if_branch :
  if state.x > 0 then state.y == state.x else state.y == 0
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "if_branch")
            .expect("property");
        let lean = prop.expression.as_deref().expect("lean rendering");
        // Lean's native if-then-else syntax. State fields prefix with `s.`
        // in Ctx::Guard.
        assert!(
            lean.contains("if s.x > 0 then s.y = s.x else s.y = 0"),
            "expected native Lean if-then-else; got: {}",
            lean
        );
    }

    #[test]
    fn if_then_else_renders_to_rust_block_form() {
        let src = r#"spec T
type State | A of { x : U64, y : U64 }
property if_branch :
  if state.x > 0 then state.y == state.x else state.y == 0
  preserved_by all
"#;
        let spec = parse_str(src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == "if_branch")
            .unwrap();
        let rust = prop.rust_expression.as_deref().expect("rust rendering");
        assert!(
            rust.contains("if s.x > 0 { s.y == s.x } else { s.y == 0 }"),
            "expected Rust block-form if-else; got: {}",
            rust
        );
    }

    // ----- `now()` builtin -----

    #[test]
    fn now_builtin_parses_in_effect() {
        let src = r#"spec NowTest
type State | Active of { last_update : U64 }
handler refresh : State.Active -> State.Active {
  permissionless
  effect { last_update := now() }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
        // Effect RHS for complex expressions is captured in Lean form
        // (consumed by lean_gen). `now()` lowers to the bare `now` symbol
        // which resolves at elaboration via QEDGen.Solana.Valid.now.
        let (_field, _kind, rhs) = h
            .effects
            .iter()
            .find(|(f, _, _)| f == "last_update")
            .expect("last_update effect");
        assert_eq!(
            rhs.trim(),
            "now",
            "Lean rendering of now() should be the bare ident `now`; got: {rhs}"
        );
    }

    #[test]
    fn now_builtin_parses_in_requires() {
        let src = r#"spec NowReq
type State | Active of { last_update : U64 }
type Error | TooSoon
handler refresh : State.Active -> State.Active {
  permissionless
  requires state.last_update + 60 <= now() else TooSoon
  effect { last_update := state.last_update + 1 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
        let req = h.requires.first().expect("requires clause");
        // Lean form references the support-library axiom by its unqualified
        // name; QEDGen.Solana.Valid.now resolves after `open QEDGen.Solana`.
        assert!(
            req.lean_expr.contains("now"),
            "lean expr should mention now; got: {}",
            req.lean_expr
        );
        assert!(
            req.rust_expr.contains("Clock::get"),
            "rust expr should mention Clock::get; got: {}",
            req.rust_expr
        );
    }

    /// `current_epoch()` parses as a zero-arg builtin: Rust
    /// `Clock::get().unwrap().epoch`, Lean bare ident `current_epoch`
    /// (axiomatized at QEDGen.Solana.Valid).
    #[test]
    fn current_epoch_builtin_parses_in_requires() {
        let src = r#"spec EpochReq
type State | Active of { last_epoch : U64 }
type Error | StaleEpoch
handler refresh : State.Active -> State.Active {
  permissionless
  requires state.last_epoch < current_epoch() else StaleEpoch
  effect { last_epoch := current_epoch() }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
        let req = h.requires.first().expect("requires clause");
        assert!(
            req.lean_expr.contains("current_epoch"),
            "lean expr should reference current_epoch; got: {}",
            req.lean_expr
        );
        assert!(
            req.rust_expr.contains("Clock::get"),
            "rust expr should mention Clock::get; got: {}",
            req.rust_expr
        );
        assert!(
            req.rust_expr.contains(".epoch"),
            "rust expr should read .epoch (not .unix_timestamp); got: {}",
            req.rust_expr
        );
    }

    // ========================================================================
    // Property classification snapshot tests
    // ========================================================================

    /// Helper: parse a tiny spec and return the named property's class.
    fn class_of(spec_src: &str, prop_name: &str) -> crate::check::PropertyClass {
        let spec = parse_str(spec_src).expect("parse");
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == prop_name)
            .unwrap_or_else(|| panic!("property `{}` not found", prop_name));
        prop.class
    }

    const CLASSIFY_SPEC_HEAD: &str = r#"
spec ClassifyTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, settled : U64, admin : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#;

    #[test]
    fn classify_property_bare_comparison_is_unary() {
        // No `old(...)`, no temporal markers — single-state predicate.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "balance_nonneg"),
            crate::check::PropertyClass::Unary
        );
    }

    #[test]
    fn classify_property_with_single_old_is_binary() {
        // `old(state.x)` anywhere ⇒ Binary — routed through the binary
        // preservation harness instead of silently lowering to `s.x >= s.x`.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "balance_monotonic"),
            crate::check::PropertyClass::Binary
        );
    }

    #[test]
    fn classify_property_with_old_under_not_is_binary() {
        // `old(...)` nested under boolean negation still triggers Binary.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property settled_changed : not (state.settled == old(state.settled)) preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "settled_changed"),
            crate::check::PropertyClass::Binary
        );
    }

    #[test]
    fn classify_property_with_old_in_implication_is_binary() {
        // `old(...)` on the LHS of an implication body — Binary.
        // Mirrors `vectors_seeded_latches_true` from pool.qedspec:694.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property latches : old(state.settled) == 1 implies state.settled == 1 preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "latches"),
            crate::check::PropertyClass::Binary
        );
    }

    #[test]
    fn classify_property_constant_body_is_unary() {
        // No state refs at all — Unary. Lowers to a constant predicate.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD, r#"property trivially_true : 1 == 1 preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "trivially_true"),
            crate::check::PropertyClass::Unary
        );
    }

    // ========================================================================
    // RustOpts.state_mode + inside_old round-trips
    // ========================================================================

    /// Helper: parse a tiny spec and render the named property's body via
    /// `expr_to_rust` under the given `RustOpts`. Returns the rendered
    /// string for assertion.
    fn render_property_body(spec_src: &str, prop_name: &str, mode: StateMode) -> String {
        let typed = crate::chumsky_parser::parse(spec_src)
            .map_err(|e| format!("parse failed: {:?}", e))
            .expect("parse");
        // Find the property in the typed AST.
        let prop_decl = typed
            .items
            .iter()
            .find_map(|item| match &item.node {
                a::TopItem::Property(p) if p.name == prop_name => Some(p),
                _ => None,
            })
            .unwrap_or_else(|| panic!("property `{}` not found in spec", prop_name));
        let env = TypeEnv::from_spec(&typed);
        let consts: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        let opts = opts_native(&env).with_state_mode(mode);
        expr_to_rust(&prop_decl.body.node, Ctx::Guard, &consts, opts)
    }

    #[test]
    fn render_unary_mode_state_x_lowers_to_s_dot_x() {
        // Today's behavior preserved under StateMode::Unary: `state.x` → `s.x`.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
        );
        let rendered = render_property_body(&src, "balance_nonneg", StateMode::Unary);
        assert!(
            rendered.contains("s.balance"),
            "expected `s.balance` in unary mode; got: {}",
            rendered
        );
        assert!(
            !rendered.contains("post.balance") && !rendered.contains("pre.balance"),
            "unary mode must not emit pre./post.; got: {}",
            rendered
        );
    }

    #[test]
    fn render_binary_mode_state_x_lowers_to_post_dot_x() {
        // Binary mode: `state.x` (no `old`) → `post.x`.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
        );
        let rendered = render_property_body(&src, "balance_nonneg", StateMode::Binary);
        assert!(
            rendered.contains("post.balance"),
            "expected `post.balance` in binary mode; got: {}",
            rendered
        );
        assert!(
            !rendered.contains("s.balance") && !rendered.contains("pre.balance"),
            "binary mode without old() must use post., not s. or pre.; got: {}",
            rendered
        );
    }

    // ========================================================================
    // Bounded `exists` over a `Fin[N]` index domain lowers to a real
    // `(0..N).any(…)` predicate; unbounded `exists` keeps the sentinel.
    // ========================================================================

    const EXISTS_SPEC_HEAD: &str = r#"
spec ExistsTest
const MAX = 8
type Idx = Fin[MAX]
type Member = { active : U64 }
type State = { members : Map[MAX] Member, count : U64 }
type Error
  | E
handler tick { effect { count := state.count + 1 } }
"#;

    #[test]
    fn render_bounded_exists_over_fin_alias_lowers_to_any() {
        let src = format!(
            "{}{}",
            EXISTS_SPEC_HEAD,
            r#"property bounded_ex : exists i : Idx, state.members[i].active == 1 preserved_by [tick]"#
        );
        let rendered = render_property_body(&src, "bounded_ex", StateMode::Unary);
        assert!(
            rendered.contains("(0..(MAX as usize)).any(|i|"),
            "bounded exists over a Fin[N] alias must iterate 0..N with .any(); got: {}",
            rendered
        );
        assert!(
            !rendered.contains(crate::check::QEDGEN_UNSUPPORTED_MARKER),
            "bounded exists must not emit the unsupported sentinel; got: {}",
            rendered
        );
    }

    #[test]
    fn render_unbounded_exists_keeps_sentinel() {
        let src = format!(
            "{}{}",
            EXISTS_SPEC_HEAD,
            r#"property unbounded_ex : exists v : U64, state.count == v preserved_by [tick]"#
        );
        let rendered = render_property_body(&src, "unbounded_ex", StateMode::Unary);
        assert!(
            rendered.contains(crate::check::QEDGEN_UNSUPPORTED_MARKER),
            "exists over an unbounded U64 domain must keep the sentinel; got: {}",
            rendered
        );
    }

    // ========================================================================
    // `ghost` lowers to a State field + per-handler update.
    // ========================================================================

    #[test]
    fn ghost_lowers_to_state_field_and_per_handler_update() {
        let src = r#"
spec G
state { balance : U64 }
type Error
  | E
ghost total : U64 {
  init { 0 }
  on mint { total := state.total + amount }
}
handler mint (amount : U64) {
  effect { balance := state.balance + amount }
}
property p : state.total == state.balance preserved_by all
"#;
        let typed = crate::chumsky_parser::parse(src).expect("parse");
        let spec = adapt(&typed);
        assert_eq!(spec.ghosts.len(), 1, "one ghost expected");
        let g = &spec.ghosts[0];
        assert_eq!(g.name, "total");
        assert_eq!(g.ty, "U64");
        assert_eq!(g.init_rust.trim(), "0");
        assert_eq!(g.updates.len(), 1);
        assert_eq!(g.updates[0].handler, "mint");
        // `state.total` resolves to `s.total` (ghost registered as a state
        // field) and the handler param `amount` is in scope.
        assert!(
            g.updates[0].value_rust.contains("s.total")
                && g.updates[0].value_rust.contains("amount"),
            "update value_rust should read the ghost + param; got {}",
            g.updates[0].value_rust
        );
        assert!(
            g.updates[0].value_lean.contains("s.total"),
            "update value_lean should read the ghost; got {}",
            g.updates[0].value_lean
        );
        // The property references the ghost as a state field.
        let prop = spec.properties.iter().find(|p| p.name == "p").unwrap();
        assert!(prop
            .rust_expression
            .as_deref()
            .is_some_and(|r| r.contains("s.total")));
    }

    // ========================================================================
    // `hook` lowers to a per-field assertion set.
    // ========================================================================

    #[test]
    fn hook_after_store_lowers_to_field_assertion() {
        let src = r#"
spec H
state { balance : U64, cap : U64 }
type Error
  | E
hook after_store(balance) {
  assert state.balance <= state.cap
}
handler deposit (amount : U64) {
  effect { balance := state.balance + amount }
}
"#;
        let typed = crate::chumsky_parser::parse(src).expect("parse");
        let spec = adapt(&typed);
        assert_eq!(spec.hooks.len(), 1);
        let h = &spec.hooks[0];
        assert!(matches!(
            &h.kind,
            crate::check::ParsedHookKind::AfterStore(f) if f == "balance"
        ));
        assert_eq!(h.asserts.len(), 1);
        assert!(
            h.asserts[0].rust.contains("s.balance") && h.asserts[0].rust.contains("s.cap"),
            "assert rust should read the state fields; got {}",
            h.asserts[0].rust
        );
    }

    #[test]
    fn hook_before_cpi_parses_with_optional_callee() {
        let src = r#"
spec H2
state { x : U64 }
type Error
  | E
hook before_cpi(Token) {
  assert state.x > 0
}
handler bump { effect { x := state.x + 1 } }
"#;
        let typed = crate::chumsky_parser::parse(src).expect("parse");
        let spec = adapt(&typed);
        assert_eq!(spec.hooks.len(), 1);
        assert!(matches!(
            &spec.hooks[0].kind,
            crate::check::ParsedHookKind::BeforeCpi(Some(c)) if c == "Token"
        ));
    }

    #[test]
    fn render_binary_mode_old_state_x_lowers_to_pre_dot_x() {
        // Binary mode: `old(state.x)` → `pre.x` — the temporal marker is
        // honored in the rendered Rust.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
        );
        let rendered = render_property_body(&src, "balance_monotonic", StateMode::Binary);
        // Expect BOTH post.balance (LHS) and pre.balance (RHS, inside old)
        // to appear — the binary obligation made explicit in the rendered
        // expression.
        assert!(
            rendered.contains("post.balance"),
            "expected `post.balance` for LHS; got: {}",
            rendered
        );
        assert!(
            rendered.contains("pre.balance"),
            "expected `pre.balance` for RHS inside old(); got: {}",
            rendered
        );
        assert!(
            !rendered.contains("s.balance"),
            "binary mode must not emit `s.balance`; got: {}",
            rendered
        );
    }

    #[test]
    fn render_unary_mode_old_collapses_to_s_dot_x() {
        // Unary path: `old(state.x)` and `state.x` both render to `s.x` —
        // the tautology shape. The vacuous-lowering lint P1s this when the
        // AST contains `Expr::Old(_)`; the path stays for compat with all
        // non-property callsites.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
        );
        let rendered = render_property_body(&src, "balance_monotonic", StateMode::Unary);
        // Both sides collapse to s.balance — the structural tautology.
        let s_count = rendered.matches("s.balance").count();
        assert!(
            s_count >= 2,
            "expected ≥2 `s.balance` (tautology shape) in unary mode; got: {} ({})",
            s_count,
            rendered
        );
    }

    #[test]
    fn classify_property_authored_tautology_no_old_is_unary() {
        // Author-written `state.x == state.x` (no `old(...)`) — Unary.
        // The vacuous-lowering lint must NOT fire on this case.
        let src = format!(
            "{}{}",
            CLASSIFY_SPEC_HEAD,
            r#"property balance_tracked : state.balance == state.balance preserved_by all"#
        );
        assert_eq!(
            class_of(&src, "balance_tracked"),
            crate::check::PropertyClass::Unary
        );
    }

    /// When state sugar (or `type State = { ... }`) is used and a handler
    /// has no explicit `accounts { ... }`, a default `state` handler-account
    /// is synthesized so guards.rs can rewrite `s.X` → `ctx.state.X`
    /// (otherwise generated guards leak raw `s.X` — compile error).
    #[test]
    fn state_sugar_handler_without_accounts_synthesizes_state_account() {
        let src = r#"spec Pool
const MAX = 4
type Error | InvalidAmount
type State = { values : Map[MAX] U64, total : U64 }

handler set_total (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { total := amt }
}

handler check_total (idx : U64) {
  requires state.values[idx] > 0 else InvalidAmount
  effect { }
}
"#;
        let spec = parse_str(src).expect("parse");
        let set_total = spec
            .handlers
            .iter()
            .find(|h| h.name == "set_total")
            .unwrap();
        let check_total = spec
            .handlers
            .iter()
            .find(|h| h.name == "check_total")
            .unwrap();

        // Effect-bearing handler: synthesized writable state account.
        assert_eq!(set_total.accounts.len(), 1);
        assert_eq!(set_total.accounts[0].name, "state");
        assert!(set_total.accounts[0].is_writable);
        assert_eq!(set_total.accounts[0].account_type.as_deref(), Some("State"));

        // Read-only handler referencing state.X via requires:
        // synthesized read-only state account.
        assert_eq!(check_total.accounts.len(), 1);
        assert_eq!(check_total.accounts[0].name, "state");
        assert!(!check_total.accounts[0].is_writable);
        assert_eq!(
            check_total.accounts[0].account_type.as_deref(),
            Some("State")
        );
    }

    /// Explicit `accounts { ... }` declarations win over the synthesis —
    /// specs declaring accounts must not pick up a stray `state` field.
    #[test]
    fn explicit_accounts_clause_suppresses_state_synthesis() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { total : U64 }

handler bump (amt : U64) {
  accounts { vault : writable }
  requires amt > 0 else InvalidAmount
  effect { total := amt }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec.handlers.iter().find(|h| h.name == "bump").unwrap();
        assert_eq!(h.accounts.len(), 1);
        assert_eq!(h.accounts[0].name, "vault");
    }

    /// Handlers that don't touch state stay account-less even when the spec
    /// has state_fields — otherwise pure helpers would silently grow a
    /// surprise state account in their Anchor instruction signature.
    #[test]
    fn no_state_synthesis_when_handler_does_not_touch_state() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { total : U64 }

handler noop (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec.handlers.iter().find(|h| h.name == "noop").unwrap();
        assert!(
            h.accounts.is_empty(),
            "noop handler should stay account-less"
        );
    }

    /// `expr_to_rust` must render `mul_div_floor` / `mul_div_ceil` with
    /// an `as u64` narrowing cast — the helpers return `u128` (the
    /// intermediate `a * b` can overflow u64), but the spec-level
    /// operation is U64 → U64. Without the cast, the canonical
    /// `let fee = mul_div_floor(total, fee_bps, BPS); let to_provider =
    /// total - fee` lowers to `u64 - u128`, which rejects at
    /// `cargo build`.
    #[test]
    fn mul_div_floor_narrows_to_u64_at_call_site() {
        let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = mul_div_floor(total, fee_bps, BPS_DENOMINATOR)
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec
            .handlers
            .iter()
            .find(|h| h.name == "accept")
            .expect("accept handler");

        // Find the `fee` binding's rendered RHS.
        let (_, _, fee_rhs) = h
            .let_bindings
            .iter()
            .find(|(name, _, _)| name == "fee")
            .expect("fee binding");

        assert!(
            fee_rhs.contains("mul_div_floor_u128"),
            "rendered RHS should still call the u128 helper for the intermediate width; got: {fee_rhs}"
        );
        assert!(
            fee_rhs.contains("as u64"),
            "rendered RHS must narrow back to u64 so downstream u64 uses (e.g. `total - fee`) typecheck; got: {fee_rhs}"
        );
    }

    /// Same contract for `mul_div_ceil`.
    #[test]
    fn mul_div_ceil_narrows_to_u64_at_call_site() {
        let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = mul_div_ceil(total, fee_bps, BPS_DENOMINATOR)
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec
            .handlers
            .iter()
            .find(|h| h.name == "accept")
            .expect("accept handler");
        let (_, _, fee_rhs) = h
            .let_bindings
            .iter()
            .find(|(name, _, _)| name == "fee")
            .expect("fee binding");
        assert!(
            fee_rhs.contains("mul_div_ceil_u128") && fee_rhs.contains("as u64"),
            "ceil variant must narrow too; got: {fee_rhs}"
        );
    }

    /// The let-binding narrow gate peels through `Paren` wrappers so the
    /// author-written `let X = (mul_div_floor(...))` shape gets the same
    /// narrowing as the bare form. Mirrors the `rust_infer_kind` peel
    /// precedent — without it, a parenthesised RHS would silently keep
    /// the u128 width and re-trigger the original `u64 - u128` mismatch.
    #[test]
    fn mul_div_in_paren_let_rhs_still_narrows_to_u64() {
        let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = (mul_div_floor(total, fee_bps, BPS_DENOMINATOR))
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = spec
            .handlers
            .iter()
            .find(|h| h.name == "accept")
            .expect("accept handler");
        let (_, _, fee_rhs) = h
            .let_bindings
            .iter()
            .find(|(name, _, _)| name == "fee")
            .expect("fee binding");
        assert!(
            fee_rhs.contains("mul_div_floor_u128") && fee_rhs.contains("as u64"),
            "parenthesised mul_div RHS must still narrow; got: {fee_rhs}"
        );
    }
}
