/-!
# Command Builders

Typed builder functions for generating Lean 4 command strings.
Each builder takes structured inputs and handles keyword quoting,
field/binder formatting, and conjunction assembly internally.

These replace ad-hoc string interpolation in the DSL elaborators
(Spec.lean, Guards.lean, Bridge.lean), centralizing the fragile
string construction into tested, single-purpose functions.

## Design

- All builders return `String` for use with `runParserCategory`
- `safeName` is the single source of truth for keyword quoting
- `mapType` is the single source of truth for DSL→Lean type mapping
- Builders never validate semantics — that stays in the elaborators
-/

namespace QEDGen.Solana.CommandBuilders

-- ============================================================================
-- Core: identifier safety and type mapping
-- ============================================================================

/-- Lean keywords that need «» quoting when used as identifiers. -/
private def leanKeywords : List String :=
  ["initialize", "open", "end", "where", "if", "then", "else", "do",
   "let", "def", "theorem", "structure", "inductive", "namespace",
   "section", "import", "return", "match", "with", "fun", "have",
   "show", "by", "from", "in", "at", "class", "instance", "deriving",
   "variable", "axiom", "opaque", "abbrev", "noncomputable", "partial",
   "unsafe", "private", "protected", "mutual", "set_option", "attribute"]

/-- Quote a name with «» if it's a Lean keyword.
    Single source of truth — replaces all `quoteName` copies. -/
def safeName (n : String) : String :=
  if leanKeywords.contains n then s!"«{n}»" else n

/-- Map DSL types to Lean types for omega compatibility.
    U64/U128/U8 → Nat, I128 → Int. Others pass through.
    Single source of truth — replaces all `mapDslType` copies. -/
def mapType (t : String) : String :=
  match t with
  | "U64"  => "Nat"
  | "U128" => "Nat"
  | "I128" => "Int"
  | "U8"   => "Nat"
  | _      => t

-- ============================================================================
-- Expression builders (produce term-level strings)
-- ============================================================================

/-- Build an explicit binder: `(name : Type)` -/
def mkBinder (name : String) (type : String) : String :=
  s!"({name} : {type})"

/-- Build a parameter signature from name/type pairs: `(a : Nat) (b : Nat)`.
    Applies `mapType` to each type. Returns empty string for empty params. -/
def mkParamSig (params : Array (String × String)) : String :=
  params.foldl (fun acc (pn, pt) => acc ++ s!" ({pn} : {mapType pt})") ""

/-- Build parameter application from name/type pairs: ` a b`.
    Returns empty string for empty params. -/
def mkParamArgs (params : Array (String × String)) : String :=
  params.foldl (fun acc (pn, _) => acc ++ s!" {pn}") ""

/-- Build a conjunction from parts: `a ∧ b ∧ c`.
    Returns empty string for empty array, single element for size 1. -/
def mkConj (parts : Array String) : String :=
  parts.foldl (fun acc p =>
    if acc.isEmpty then p else acc ++ s!" ∧ {p}") ""

/-- Build a record update: `some { base with x := 1, y := 2 }`.
    Returns `some base` for empty assigns. -/
def mkSomeUpdate (base : String) (assigns : Array String) : String :=
  if assigns.isEmpty then
    s!"some {base}"
  else
    let body := assigns.foldl (fun acc a =>
      if acc.isEmpty then a else acc ++ s!", {a}") ""
    s!"some \{ {base} with {body} }"

-- ============================================================================
-- Command builders (produce full command strings)
-- ============================================================================

/-- `namespace Foo` -/
def mkNamespace (name : String) : String :=
  s!"namespace {name}"

/-- `end Foo` -/
def mkEnd (name : String) : String :=
  s!"end {name}"

/-- `open Foo.Bar` -/
def mkOpen (path : String) : String :=
  s!"open {path}"

/-- `abbrev Foo : Type := value` -/
def mkAbbrev (name : String) (type : String) (value : String) : String :=
  s!"abbrev {safeName name} : {type} := {value}"

/-- `def Foo : Type := value` — simple definition with no params. -/
def mkSimpleDef (name : String) (type : String) (value : String) : String :=
  s!"def {safeName name} : {type} := {value}"

/-- `def Foo (sig parts) : RetType :=\n  body` — definition with arbitrary signature.
    `sig` is the full parameter + return type string (e.g., "(s : State) : Option State").
    `body` is the definition body (may be multiline). -/
def mkDef (name : String) (sig : String) (body : String) : String :=
  s!"def {safeName name} {sig} :=\n{body}"

/-- Build a structure declaration from name and field list.
    Fields are `(fieldName, leanType)` pairs — `safeName` applied to field names,
    `mapType` applied to types.
    ```
    structure Foo where
      x : Nat
      y : Bool
      deriving Repr, DecidableEq, BEq
    ``` -/
def mkStructure (name : String) (fields : Array (String × String))
    (deriving_ : Array String := #["Repr", "DecidableEq", "BEq"]) : String :=
  let fieldStr := fields.foldl (fun acc (fn_, ft) =>
    acc ++ s!"  {safeName fn_} : {mapType ft}\n") ""
  let derivStr := if deriving_.isEmpty then ""
    else "  deriving " ++ deriving_.foldl (fun acc d =>
      if acc.isEmpty then d else acc ++ s!", {d}") ""
  s!"structure {safeName name} where\n{fieldStr}{derivStr}"

/-- Build an inductive declaration from name and constructor names.
    ```
    inductive Status where
      | Open | Closed
      deriving Repr, DecidableEq, BEq
    ``` -/
def mkInductive (name : String) (ctors : Array String)
    (deriving_ : Array String := #["Repr", "DecidableEq", "BEq"]) : String :=
  let variants := ctors.foldl (fun acc s => acc ++ s!" | {s}") ""
  let derivStr := if deriving_.isEmpty then ""
    else "\n  deriving " ++ deriving_.foldl (fun acc d =>
      if acc.isEmpty then d else acc ++ s!", {d}") ""
  s!"inductive {safeName name} where{variants}{derivStr}"

/-- Build a theorem with sorry body.
    `binders` are pre-formatted strings like `"(s : State)"` or `"(h : foo ≠ none)"`.
    `conclusion` is the goal type.
    ```
    theorem foo (s : State) (h : bar ≠ none) :
        conclusion := sorry
    ``` -/
def mkSorryTheorem (name : String) (binders : Array String)
    (conclusion : String) : String :=
  let binderStr := binders.foldl (fun acc b => acc ++ s!" {b}") ""
  s!"theorem {safeName name}{binderStr} :\n    {conclusion} := sorry"

/-- Build a sorry theorem with a preceding `/-- ... -/` doc comment.
    `doc` is the intent gloss (e.g., "Only the owner can withdraw").
    The doc comment is emitted as a separate command before the theorem
    so `runParserCategory` can parse each independently. -/
def mkDocSorryTheorem (name : String) (binders : Array String)
    (conclusion : String) (doc : String) : String :=
  let binderStr := binders.foldl (fun acc b => acc ++ s!" {b}") ""
  s!"/-- {doc} -/\ntheorem {safeName name}{binderStr} :\n    {conclusion} := sorry"

/-- Build a module doc comment block: `/-! ... -/`.
    Used for assumption summaries at the top of generated namespaces. -/
def mkModuleDoc (content : String) : String :=
  s!"/-!\n{content}\n-/"

/-- Build a theorem with a tactic proof body.
    Like `mkSorryTheorem` but with `by\n  tactics` instead of `sorry`. -/
def mkTacticTheorem (name : String) (binders : Array String)
    (conclusion : String) (tactics : String)
    (options : String := "") : String :=
  let binderStr := binders.foldl (fun acc b => acc ++ s!" {b}") ""
  let optLine := if options.isEmpty then "" else s!"{options}\n"
  s!"{optLine}theorem {safeName name}{binderStr} :\n    {conclusion} := by\n  {tactics}"

end QEDGen.Solana.CommandBuilders
