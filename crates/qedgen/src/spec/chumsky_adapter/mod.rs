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

// Per-concern submodules. The directory rename keeps the module path
// `crate::spec::chumsky_adapter` (and the root re-export
// `crate::chumsky_adapter`) intact; these globs re-export each submodule's
// items so the existing `crate::chumsky_adapter::<name>` call sites — and the
// cross-submodule references — continue to resolve unchanged.
mod adapt;
mod canon;
mod effects;
mod lean;
mod rust;
mod tree;
mod typecheck;

pub use adapt::{adapt, parse_str};
pub use typecheck::typecheck_spec;

pub(in crate::spec::chumsky_adapter) use canon::*;
pub(crate) use canon::{collect_guard_path_refs, GuardPathRef};
pub(in crate::spec::chumsky_adapter) use effects::*;
pub(in crate::spec::chumsky_adapter) use lean::*;
pub(in crate::spec::chumsky_adapter) use rust::*;
pub(in crate::spec::chumsky_adapter) use tree::*;
pub(in crate::spec::chumsky_adapter) use typecheck::collect_uninterpreted_helpers;

#[cfg(test)]
mod tests;

// ============================================================================
// Shared rendering context: Ctx / Kind / ConstTable / TypeEnv
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
    external_fields:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, &'a a::TypeRef>>,
    aliases: std::collections::BTreeMap<String, String>,
    /// Sum-type registry: enum name → (variant name → its Rust
    /// [`VariantShape`]). `Struct` (`Approved of { … }` → `Enum::Approved { .. }`),
    /// `Tuple` (`Custom of I64` → `Enum::Custom(..)`), or `Unit` (`Executing` →
    /// `Enum::Executing`). Populated from every `TopItem::Adt`. Consumed by
    /// [`Self::resolve_variant`] to render an `is .Variant` test as a
    /// shape-correct `matches!` pattern.
    adts: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, crate::mir::VariantShape>,
    >,
}

/// Classify a variant's declared fields into its Rust [`VariantShape`]. No
/// fields → `Unit`; all-numeric field names (the parser's `"0"`, `"1"`, … tuple
/// marker — a real field's grammar requires an identifier) → `Tuple`; otherwise
/// named fields → `Struct`.
fn variant_shape(fields: &[a::TypedField]) -> crate::mir::VariantShape {
    use crate::mir::VariantShape;
    if fields.is_empty() {
        VariantShape::Unit
    } else if fields.iter().all(|f| f.name.parse::<usize>().is_ok()) {
        VariantShape::Tuple
    } else {
        VariantShape::Struct
    }
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
                    let mut shapes = std::collections::BTreeMap::new();
                    for variant in &a.variants {
                        shapes.insert(variant.name.clone(), variant_shape(&variant.fields));
                        for f in &variant.fields {
                            env.state_fields.entry(f.name.clone()).or_insert(&f.ty);
                        }
                    }
                    env.adts.insert(a.name.clone(), shapes);
                }
                TopItem::TypeAlias(ta) => {
                    env.aliases
                        .insert(ta.name.clone(), type_ref_to_string(&ta.target));
                }
                TopItem::Dimension(dimension) => {
                    env.aliases
                        .insert(dimension.name.clone(), type_ref_to_string(&dimension.base));
                }
                // Ghosts render as state fields: `state.<ghost>` must resolve
                // in properties / invariants / `requires` / `ensures` and in
                // other ghosts' update RHS. They are rendering-only here — the
                // on-chain codegen reads `ParsedSpec.state_fields`, which never
                // includes ghosts.
                TopItem::Ghost(g) => {
                    env.state_fields.entry(g.name.clone()).or_insert(&g.ty);
                }
                TopItem::Environment(environment) => {
                    for clause in &environment.clauses {
                        if let a::EnvClause::External { object, field, ty } = &clause.node {
                            env.external_fields
                                .entry(object.clone())
                                .or_default()
                                .insert(field.clone(), ty);
                        }
                    }
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
        let resolved = self.resolve_alias_name(binder_ty);
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
            a::TypeRef::Named(n) => match self.resolve_alias_name(n).as_str() {
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

    fn resolve_alias_name(&self, name: &str) -> String {
        let mut resolved = name.trim().to_string();
        let mut seen = std::collections::BTreeSet::new();
        while seen.insert(resolved.clone()) {
            let Some(next) = self.aliases.get(&resolved) else {
                break;
            };
            resolved = next.trim().to_string();
        }
        resolved
    }

    /// Shared walking core for [`Self::path_kind`] / [`Self::path_type_name`]
    /// / [`Self::path_type_ref`] and the tree builder's `path_leaf_type`:
    /// resolve the leaf `TypeRef` of a path. State-rooted paths walk the
    /// segments — the first Field must be a state field; subsequent Fields
    /// index into a record or Map-of-record; a subscript advances to the
    /// Map's inner type. Bare idents resolve through the handler params;
    /// everything else is untyped.
    ///
    /// `record_state_fallback` additionally resolves first-segment fields
    /// through `records["State"]` (record-form state: `type State = { … }` /
    /// `state { … }` sugar keeps its fields there, not in `state_fields`).
    /// Only `path_leaf_type` passes `true` — widening the others would
    /// change `path_is_pod_field`'s answers and with them the rendered
    /// Quasar output.
    fn resolve_path_leaf(
        &self,
        p: &a::Path,
        record_state_fallback: bool,
    ) -> Option<&'a a::TypeRef> {
        if p.root == "state" {
            let mut current: Option<&a::TypeRef> = None;
            for seg in &p.segments {
                match seg {
                    a::PathSeg::Field(f) => {
                        current = match current {
                            None => {
                                let direct = self.state_fields.get(f).copied();
                                if record_state_fallback {
                                    direct.or_else(|| {
                                        self.records.get("State").and_then(|m| m.get(f).copied())
                                    })
                                } else {
                                    direct
                                }
                            }
                            Some(a::TypeRef::Named(rec)) => {
                                self.records.get(rec).and_then(|m| m.get(f).copied())
                            }
                            // direct .field after a Map without [idx] shouldn't happen
                            // in valid specs, but bottom out safely
                            Some(a::TypeRef::Map { inner, .. }) => match inner.as_ref() {
                                a::TypeRef::Named(rec) => {
                                    self.records.get(rec).and_then(|m| m.get(f).copied())
                                }
                                _ => None,
                            },
                            _ => None,
                        };
                    }
                    a::PathSeg::Index(_) => {
                        // Subscript into a Map: advance `current` to the inner type.
                        if let Some(a::TypeRef::Map { inner, .. }) = current {
                            current = Some(inner.as_ref());
                        }
                    }
                }
            }
            return current;
        }
        if let Some(fields) = self.external_fields.get(&p.root) {
            let mut current: Option<&a::TypeRef> = None;
            for seg in &p.segments {
                match seg {
                    a::PathSeg::Field(field) => {
                        current = match current {
                            None => fields.get(field).copied(),
                            Some(a::TypeRef::Named(record)) => {
                                self.records.get(record).and_then(|m| m.get(field).copied())
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
            return current;
        }
        // Bare ident — try handler params.
        if p.segments.is_empty() {
            return self
                .params
                .iter()
                .find(|(n, _)| n == &p.root)
                .map(|(_, t)| *t);
        }
        None
    }

    fn is_external_root(&self, name: &str) -> bool {
        self.external_fields.contains_key(name)
    }

    /// Resolve the kind of a Path. Handles subscripts into Map fields by
    /// reading through the map's value-record to find the trailing field.
    fn path_kind(&self, p: &a::Path) -> Kind {
        self.resolve_path_leaf(p, false)
            .map(|t| self.type_ref_kind(t))
            .unwrap_or(Kind::Nat)
    }

    /// Resolve the SOURCE type name of a path expression — e.g.,
    /// `state.accounts[i]` → `"Account"` when `accounts : Map[N] Account`.
    /// Returns None when the path terminates on a primitive/Bool/unknown type
    /// or doesn't refer into the state.
    fn path_type_name(&self, p: &a::Path) -> Option<String> {
        match self.resolve_path_leaf(p, false)? {
            a::TypeRef::Named(n) => Some(n.clone()),
            // `Option T` / `Vec T` etc. — the constructor names the enum for
            // variant resolution (`Option` → `Some`/`None` in a `match`).
            a::TypeRef::Param(ctor, _) => Some(ctor.clone()),
            _ => None,
        }
    }

    /// Resolve an `is .Variant` test to `(enum_name, is_struct_variant)` for
    /// shape-correct Rust `matches!` rendering. `type_hint` is the scrutinee's
    /// resolved type name (e.g. `state.status : ProposalStatus`) when known —
    /// the enum is looked up there first. When the hint is absent or doesn't
    /// carry the variant, fall back to a global search: a variant name unique
    /// across all registered sum types resolves unambiguously; an ambiguous or
    /// unknown name returns `None` (caller keeps a best-effort render).
    fn resolve_variant(
        &self,
        type_hint: Option<&str>,
        variant: &str,
    ) -> Option<(String, crate::mir::VariantShape)> {
        use crate::mir::VariantShape;
        // Builtin `Option` (prelude) — `Some(x)` is a tuple variant, `None` a
        // unit variant; both usable as `Option::Some` / `Option::None`. Lets a
        // predicate match an `Option` field (`match state.x with | Some h => …`).
        match variant {
            "Some" => return Some(("Option".to_string(), VariantShape::Tuple)),
            "None" => return Some(("Option".to_string(), VariantShape::Unit)),
            _ => {}
        }
        if let Some(enum_name) = type_hint {
            if let Some(shapes) = self.adts.get(enum_name) {
                if let Some(&shape) = shapes.get(variant) {
                    return Some((enum_name.to_string(), shape));
                }
            }
        }
        let mut hits = self
            .adts
            .iter()
            .filter_map(|(name, shapes)| shapes.get(variant).map(|&s| (name.clone(), s)));
        let first = hits.next()?;
        // Ambiguous — the same variant name in two enums; can't disambiguate
        // without the scrutinee type, so decline rather than guess wrong.
        if hits.next().is_some() {
            return None;
        }
        Some(first)
    }

    /// Infer the nominal record a `{ field := …, … }` literal constructs
    /// (#325): the unique declared record whose field-name set exactly
    /// matches the literal's. Exact-set matching keeps inference honest —
    /// a partial literal is a spec error the `unresolved_constructor_type`
    /// lint reports, not something to guess through. Ambiguity (two
    /// records with identical field sets) declines rather than guessing.
    /// The flat-state `State` mirror record is excluded — a literal is a
    /// value expression, not a state constructor.
    fn record_for_fields(&self, field_names: &[&str]) -> Option<String> {
        let want: std::collections::BTreeSet<&str> = field_names.iter().copied().collect();
        let mut hits = self.records.iter().filter_map(|(name, fields)| {
            if name == "State" {
                return None;
            }
            let have: std::collections::BTreeSet<&str> =
                fields.keys().map(|k| k.as_str()).collect();
            (have == want).then(|| name.clone())
        });
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some(first)
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
            Expr::QuantIn { .. } => Kind::Bool,
            Expr::BoolOp { .. } => Kind::Bool,
            Expr::Not(_) => Kind::Bool,
            Expr::Cmp { .. } => Kind::Bool,
            Expr::Contains { .. } => Kind::Bool,
            Expr::Len(_) => Kind::Nat,
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
            Expr::MulDivFloor { a, b, .. }
            | Expr::MulDivCeil { a, b, .. }
            | Expr::MulDivRoundHalfUp { a, b, .. } => {
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
                self.resolve_alias_name(n).as_str(),
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
        self.resolve_path_leaf(p, false)
    }
}

/// Any `Expr::Old(_)` in the tree? Used by `classify_property_body` and the
/// `vacuous_property_lowering` lint. Walks the shared `ast::for_each_child`
/// spine (F2), keeping only the `Old` arm.
pub(crate) fn expr_contains_old(node: &Node<Expr>) -> bool {
    if matches!(node.node, Expr::Old(_)) {
        return true;
    }
    let mut found = false;
    crate::ast::for_each_child(&node.node, |child| {
        found = found || expr_contains_old(child);
    });
    found
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
