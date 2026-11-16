//! `record-shape`: a signature or literal that leaves a record unnamed — a
//! bare dict where a class belongs. The family's own rules: signatures
//! (grab-bags, collections of records, fixed tuples; maps pass) and literals
//! (>= 2 keys, >= 1 constant key, >= 1 dynamic value, in a record position).
//!
//! A wire payload is still a record: serialisation is no excuse for skipping
//! the class — write a serialiser method on the class if the boundary needs
//! one.
// lucidlint: ignore-file complexity the annotation and literal walkers are ONE
// exhaustive match over the typing surface — the arm count is the AST's shape, not
// branching; checks.rs carried this why before the split, and the code is unchanged
// lucidlint: ignore-file large-function the family walker stays one mapping so every
// record position is handled in one place — the alternative is six partial walkers

use std::collections::HashSet;

use ruff_python_ast::{AnyNodeRef, Expr, Operator, Stmt, StmtFunctionDef, UnaryOp};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::col_of, source::line_of, ScanState};

use super::{class_attr_names, class_field_names, param_is_mutated, push_stmt_children, skel_children, stmt_exprs, Q};

/// `_name_of`: the bare name of an annotation (typing.Any -> Any).
fn ann_name_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        Expr::NoneLiteral(_) => Some("None".to_string()),
        Expr::BooleanLiteral(b) => Some(b.value.to_string()),
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        _ => None,
    }
}

/// `_base_name`: lowercase dict/list/tuple/optional spellings (typing.Dict -> dict).
fn ann_base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_lowercase()),
        Expr::Attribute(a) => Some(a.attr.to_lowercase()),
        _ => None,
    }
}

/// G1: is this dict VALUE element class-name-like — a capitalized name or
/// attribute (leading underscores ignored: `_TflJourneyResponse` is a
/// class). A map whose values are classes holds shaped objects, not
/// scalars, so the dict is a record.
fn ann_value_is_class_like(e: &Expr) -> bool {
    let name = match e {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return false,
    };
    name.trim_start_matches('_')
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase())
}

/// (P1) The module's type bindings — the names a dict value element may
/// resolve to: classes defined in the module, import aliases (`from decimal
/// import Decimal` binds `Decimal`; `import json` binds `json`), and the
/// fixed typing/builtin constructor set. Classification is AST name
/// resolution, never capitalization.
pub(super) struct TypeBindings {
    names: std::collections::HashSet<String>,
    /// (P1) Module-level raw-dict aliases: `Sources = dict[str, Provenance]`
    /// (or `Sources: TypeAlias = ...`). An alias names the shape but carries
    /// no behavior, so it must not exempt the collection finding — the
    /// annotation resolves through it to the dict it names.
    aliases: std::collections::HashMap<String, Expr>,
}

/// (P1) The fixed typing/builtin constructor names — a name ANY module may
/// use without importing (builtin constructors) or a typing constructor
/// that is conventional enough to recognize unimported (Mapping, Sequence,
/// ...). Lowercase builtins included so resolution — never capitalization —
/// is what admits them. Scalar builtins (str/int/float/bool/bytes) stay on
/// the G1 scalar-map path and are deliberately absent here.
const KNOWN_TYPE_CONSTRUCTORS: &[&str] = &[
    "list",
    "dict",
    "tuple",
    "set",
    "frozenset",
    "bytearray",
    "complex",
    "range",
    "slice",
    "memoryview",
    "type",
    "Any",
    "Optional",
    "Union",
    "Mapping",
    "MutableMapping",
    "Sequence",
    "MutableSequence",
    "Iterable",
    "Iterator",
    "Collection",
    "Container",
    "Hashable",
    "Awaitable",
    "Coroutine",
    "Generator",
    "AsyncIterable",
    "AsyncIterator",
    "Callable",
    "Type",
    "Literal",
    "ClassVar",
    "Final",
    "NamedTuple",
    "TypedDict",
    "Protocol",
    "Generic",
    "TypeVar",
    "Self",
    "Never",
    "NoReturn",
    "List",
    "Dict",
    "Set",
    "Tuple",
    "FrozenSet",
];

/// (P1) Build the binding table from the module's own AST: every ClassDef
/// name, every Import/ImportFrom alias (the AS name, else the imported
/// name — `import decimal` binds `decimal`, `import x.y as z` binds `z`),
/// plus the fixed constructor set. Module-level type aliases
/// (`Sources = dict[str, Provenance]`) are recorded separately so an
/// annotation names the shape it aliases.
pub(super) fn module_type_bindings(body: &[Stmt]) -> TypeBindings {
    let mut names: std::collections::HashSet<String> = KNOWN_TYPE_CONSTRUCTORS.iter().map(|s| s.to_string()).collect();
    let mut aliases: std::collections::HashMap<String, Expr> = std::collections::HashMap::new();
    for s in body {
        match s {
            Stmt::Assign(a) if a.targets.len() == 1 => {
                if let (Expr::Name(n), true) = (&a.targets[0], type_alias_value(&a.value)) {
                    aliases.insert(n.id.to_string(), (*a.value).clone());
                }
            }
            Stmt::AnnAssign(a) => {
                if let (Expr::Name(n), Some(v)) = (a.target.as_ref(), a.value.as_deref()) {
                    if type_alias_value(v) {
                        aliases.insert(n.id.to_string(), v.clone());
                    }
                }
            }
            _ => {}
        }
    }
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        match s {
            Stmt::ClassDef(c) => {
                names.insert(c.name.to_string());
            }
            Stmt::Import(i) => {
                for a in &i.names {
                    let bound = match &a.asname {
                        Some(n) => n.to_string(),
                        None => a.name.split('.').next().unwrap_or(a.name.as_str()).to_string(),
                    };
                    names.insert(bound);
                }
            }
            Stmt::ImportFrom(im) => {
                for a in &im.names {
                    if a.name.as_str() != "*" {
                        let bound = match &a.asname {
                            Some(n) => n.to_string(),
                            None => a.name.to_string(),
                        };
                        names.insert(bound);
                    }
                }
            }
            _ => {}
        }
        push_stmt_children(s, &mut sq);
    }
    TypeBindings { names, aliases }
}

/// (P1) Is this assignment value a type expression (a raw-dict alias such as
/// `dict[str, Provenance]`), not a runtime value? Subscripts, names,
/// attributes, and unions qualify; literals and calls do not.
fn type_alias_value(e: &Expr) -> bool {
    match e {
        Expr::Subscript(_) | Expr::Name(_) | Expr::Attribute(_) => true,
        Expr::BinOp(b) => matches!(b.op, Operator::BitOr),
        _ => false,
    }
}

/// (P1) Resolve an annotation through the module's raw-dict aliases:
/// `Sources = dict[str, Provenance]` makes `sources: Sources` the same
/// shape as `sources: dict[str, Provenance]`. One name that is not an
/// alias (or a cycle, bounded) ends the walk.
pub(super) fn resolve_alias<'a>(e: &'a Expr, bindings: &'a TypeBindings) -> &'a Expr {
    let mut cur = e;
    for _ in 0..8 {
        match cur {
            Expr::Name(n) => match bindings.aliases.get(n.id.as_str()) {
                Some(target) => cur = target,
                None => return cur,
            },
            _ => return cur,
        }
    }
    cur
}

/// (P1) Is this annotation-name a named type? First-class rule: the name
/// resolves through the module's binding table (or is a raw-dict alias,
/// which resolves to the dict it names). The capitalization check is the
/// retained H1 fallback so an unbound capitalized name keeps its existing
/// behavior — resolution is the criterion for NEW classifications.
fn ann_value_is_named_type(e: &Expr, bindings: &TypeBindings) -> bool {
    let e = resolve_alias(e, bindings);
    match e {
        Expr::Name(n) => bindings.names.contains(n.id.as_str()) || ann_value_is_class_like(e),
        Expr::Attribute(a) => bindings.names.contains(a.attr.as_str()) || ann_value_is_class_like(e),
        _ => false,
    }
}

///(P1) The value class of a dict-shaped annotation: `dict[str, SomeClass]`
/// — the subscript's value element names a type the module table resolves
/// (a module class, an imported name, a known typing/builtin constructor)
/// OR keeps the H1 class-like fallback for unbound capitalized names; typed
/// scalars excepted (G1). The collection is the unnamed shape, so the
/// finding names the map — never the value, which the annotation already
/// names. A union over the annotation (`dict[str, Provenance] | None`) is
/// resolved to its single dict member, so the same shape renders the same
/// message in every arm; a union value, a nested collection, or a non-dict
/// annotation returns None (those keep the ad-hoc/union texts).
fn map_value_class(a: &Expr, bindings: &TypeBindings) -> Option<String> {
    let a = resolve_alias(a, bindings);
    let mut wrapped = Vec::new();
    ann_unwrap(a, &mut wrapped);
    let mut found: Vec<String> = Vec::new();
    for part in &wrapped {
        if let Some(record) = map_value_class_one(part, bindings) {
            found.push(record);
        }
    }
    if found.len() == 1 {
        Some(found.remove(0))
    } else {
        None // no dict member (not a map) or several (ambiguous)
    }
}

/// (P1) One annotation member's value class: `dict[str, SomeClass]` only.
fn map_value_class_one(e: &Expr, bindings: &TypeBindings) -> Option<String> {
    let Expr::Subscript(s) = e else {
        return None;
    };
    if ann_base_name(&s.value).as_deref() != Some("dict") {
        return None;
    }
    let Expr::Tuple(t) = s.slice.as_ref() else {
        return None;
    };
    if t.elts.len() != 2 {
        return None;
    }
    if !matches!(ann_name_of(&t.elts[0]).as_deref(), Some("str" | "Any")) {
        return None;
    }
    let mut val_parts = Vec::new();
    ann_unwrap(&t.elts[1], &mut val_parts);
    if val_parts.len() != 1 {
        return None;
    }
    let val = resolve_alias(val_parts[0], bindings);
    // grab-bag markers are not classes — dict[str, Any] keeps the ad-hoc text
    if matches!(ann_name_of(val).as_deref(), Some("Any" | "object")) {
        return None;
    }
    if matches!(val, Expr::Subscript(_)) {
        return None; // collection values keep the fixed-shape text
    }
    if matches!(
        ann_name_of(val).as_deref(),
        Some("str" | "int" | "float" | "bool" | "bytes" | "None")
    ) {
        return None; // G1 scalar-map: a lookup, not a record
    }
    if ann_value_is_named_type(val, bindings) {
        return ann_name_of(val);
    }
    None
}

/// `_unwrap`: peel Optional[..]/Union[..]/A | B wrappers into their members.
fn ann_unwrap<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match e {
        Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
            ann_unwrap(&b.left, out);
            ann_unwrap(&b.right, out);
        }
        Expr::Subscript(s) if matches!(ann_base_name(&s.value).as_deref(), Some("optional" | "union")) => {
            let slice: &Expr = s.slice.as_ref();
            if let Expr::Tuple(t) = slice {
                for elt in &t.elts {
                    ann_unwrap(elt, out);
                }
            } else {
                ann_unwrap(slice, out);
            }
        }
        _ => out.push(e),
    }
}

/// `_is_variadic_tuple`: tuple[T, ...] is a homogeneous sequence, not a pair.
fn is_variadic_tuple(e: &Expr) -> bool {
    if let Expr::Subscript(s) = e {
        if ann_base_name(&s.value).as_deref() == Some("tuple") {
            if let Expr::Tuple(t) = s.slice.as_ref() {
                return t.elts.iter().any(|elt| matches!(elt, Expr::EllipsisLiteral(_)));
            }
        }
    }
    false
}

/// `_annotation_is_record`: record-shaped bare collection in a signature.
/// Raw-dict aliases resolve first, so `sources: Sources` is the record its
/// alias names.
fn annotation_is_record(e: &Expr, bindings: &TypeBindings) -> bool {
    let e = resolve_alias(e, bindings);
    let mut wrapped = Vec::new();
    ann_unwrap(e, &mut wrapped);
    if wrapped.len() != 1 {
        return wrapped.iter().any(|p| annotation_is_record(p, bindings));
    }
    let node = wrapped[0];
    if let Expr::Name(n) = node {
        return n.id.eq_ignore_ascii_case("dict"); // bare dict = grab-bag
    }
    if let Expr::Subscript(s) = node {
        let base = ann_base_name(&s.value);
        if base.as_deref() == Some("dict") {
            let slice: &Expr = s.slice.as_ref();
            if let Expr::Tuple(t) = slice {
                if t.elts.len() != 2 {
                    return false; // malformed — tolerate
                }
                let key = &t.elts[0];
                if !matches!(ann_name_of(key).as_deref(), Some("str" | "Any")) {
                    return false;
                }
                let mut val_parts = Vec::new();
                ann_unwrap(&t.elts[1], &mut val_parts);
                if val_parts.len() > 1 {
                    // a union value: a record or shapeless member makes it a
                    // record; an all-scalar union (`str | None`) is a lookup
                    return val_parts.iter().any(|p| annotation_is_record(p, bindings))
                        || val_parts
                            .iter()
                            .any(|p| matches!(ann_name_of(p).as_deref(), Some("Any" | "object")))
                        || val_parts.iter().any(|p| ann_value_is_class_like(p));
                }
                let val = val_parts[0];
                let val_name = ann_name_of(val);
                if matches!(val_name.as_deref(), Some("Any" | "object")) {
                    return true; // grab-bag: no shape
                }
                // G1 scalar-map: a keyed collection of SCALAR values is a
                // lookup, not a record — dict[str, str] stamps, dict[str,
                // int] counts, dict[str, None] sentinels hold no shape. A
                // class-name-like value or a nested collection keeps the
                // finding (the dict holds shape).
                if matches!(
                    val_name.as_deref(),
                    Some("str" | "int" | "float" | "bool" | "bytes" | "None")
                ) {
                    return false;
                }
                if matches!(val, Expr::Subscript(_)) {
                    return !is_variadic_tuple(val); // collection values are records
                }
                if ann_value_is_class_like(val) {
                    return true; // class-named values keep the finding
                }
                return matches!(ann_base_name(val).as_deref(), Some("dict" | "tuple" | "list"));
            }
            return true; // dict[X] single-arg or dict[()]
        }
        if base.as_deref() == Some("tuple") {
            return !is_variadic_tuple(node); // fixed-size pairs are records
        }
        if base.as_deref() == Some("list") {
            let mut parts = Vec::new();
            ann_unwrap(s.slice.as_ref(), &mut parts);
            if parts.len() != 1 {
                return parts.iter().any(|p| annotation_is_record(p, bindings));
            }
            let value = parts[0];
            if matches!(value, Expr::Subscript(_)) {
                return !is_variadic_tuple(value);
            }
            return matches!(
                value,
                Expr::Name(n) if matches!(n.id.to_lowercase().as_str(), "dict" | "tuple" | "list")
            );
        }
    }
    false
}

/// The annotation's collection form for the record-shape message: the
/// message must name the shape the reader wrote (fixed tuple, list), never
/// hard-code "dict". A union takes its record member's form.
#[derive(Clone, Copy, PartialEq)]
enum SignatureRecordShape {
    Dict,
    FixedTuple,
    List,
}

fn signature_record_shape(e: &Expr, bindings: &TypeBindings) -> SignatureRecordShape {
    let e = resolve_alias(e, bindings);
    let mut wrapped = Vec::new();
    ann_unwrap(e, &mut wrapped);
    for node in wrapped {
        if let Expr::Subscript(s) = node {
            match ann_base_name(&s.value).as_deref() {
                Some("tuple") if !is_variadic_tuple(node) => return SignatureRecordShape::FixedTuple,
                Some("list") => return SignatureRecordShape::List,
                _ => {}
            }
        }
    }
    SignatureRecordShape::Dict
}

/// A SCALAR LITERAL value: a string, number, bool, or None literal (unary
/// +/- on a number stays scalar — `-1` IS the literal -1). A dict whose
/// every value is scalar is a lookup keyed by identity, not a record
/// (P1 scalar-map); values that are collections, calls, names, or attribute
/// loads carry shape and keep the finding.
fn is_scalar_literal(e: &Expr) -> bool {
    match e {
        Expr::StringLiteral(_) | Expr::NumberLiteral(_) | Expr::BooleanLiteral(_) | Expr::NoneLiteral(_) => true,
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::UAdd | UnaryOp::USub) => is_scalar_literal(&u.operand),
        _ => false,
    }
}

/// (P1) The record-literal value's one named class: every value element
/// must be a name/attribute that resolves through the module's binding
/// table (or stays class-like), and all must name the SAME type, for the
/// literal to render the collection message. A dict whose values are
/// scalars, calls, collections, or mixed classes keeps the constant-keys
/// text (an unresolvable value element keeps the existing behavior).
fn literal_value_record<'a>(vals: impl Iterator<Item = &'a Expr>, bindings: &TypeBindings) -> Option<String> {
    let mut record: Option<String> = None;
    for v in vals {
        let name = match v {
            Expr::Name(n) => n.id.to_string(),
            Expr::Attribute(a) => a.attr.to_string(),
            _ => return None,
        };
        if matches!(
            name.as_str(),
            "Any" | "object" | "str" | "int" | "float" | "bool" | "bytes" | "None"
        ) {
            return None; // scalars and grab-bags are not named types
        }
        if !ann_value_is_named_type(v, bindings) {
            return None; // an unresolvable element keeps the existing text
        }
        match &record {
            Some(r) if r != &name => return None, // mixed classes: no single record
            None => record = Some(name),
            _ => {}
        }
    }
    record
}

/// The record-dict hit: shape and keys for the record-shape message; spread
/// merges ({**base, ...}) update an existing shape and are not records.
/// `record` is the collection message's value class when every value names
/// it (P1), else None (the constant-keys text).
struct RecordHit {
    line: usize,
    col: usize,
    keys: Vec<String>,
    record: Option<String>,
}

/// The dict-literal scan: SHAPE, not position, defines a record — every
/// expression container is descended (call arguments included; the old
/// "inline arguments are maps" carve-out hid wire-format construction sites).
/// Spread merges ({**base, ...}) update an existing shape and are not records.
fn record_literal_scan(e: &Expr, source: &str, bindings: &TypeBindings, found: &mut Vec<RecordHit>) {
    match e {
        Expr::Dict(d) => {
            if d.items.iter().any(|it| it.key.is_none()) {
                return;
            }
            let keys: Vec<String> = d
                .items
                .iter()
                .filter_map(|it| match &it.key {
                    Some(Expr::StringLiteral(s)) => Some(s.value.to_string()),
                    _ => None,
                })
                .collect();
            // scalar-map: EVERY value a scalar literal — a lookup keyed by
            // identity, not a record. A collection/call/name/attribute value
            // keeps the finding (the dict holds shape).
            let scalar_map = d.items.iter().all(|it| is_scalar_literal(&it.value));
            if is_literal_record(d, &keys, scalar_map) {
                let record = literal_value_record(d.items.iter().map(|it| &it.value), bindings);
                found.push(RecordHit {
                    line: line_of(source, d.range().start()),
                    col: col_of(source, d.range().start()),
                    keys,
                    record,
                });
            }
            for it in &d.items {
                record_literal_scan(&it.value, source, bindings, found);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                record_literal_scan(elt, source, bindings, found);
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                record_literal_scan(elt, source, bindings, found);
            }
        }
        Expr::Set(s) => {
            for elt in &s.elts {
                record_literal_scan(elt, source, bindings, found);
            }
        }
        Expr::If(ie) => {
            record_literal_scan(&ie.body, source, bindings, found);
            record_literal_scan(&ie.orelse, source, bindings, found);
        }
        Expr::ListComp(c) => record_literal_scan(&c.elt, source, bindings, found),
        Expr::SetComp(c) => record_literal_scan(&c.elt, source, bindings, found),
        Expr::Generator(c) => record_literal_scan(&c.elt, source, bindings, found),
        Expr::DictComp(c) => {
            if let Some(k) = &c.key {
                record_literal_scan(k, source, bindings, found);
            }
            record_literal_scan(&c.value, source, bindings, found);
        }
        Expr::Lambda(l) => record_literal_scan(&l.body, source, bindings, found),
        Expr::Call(c) => {
            // dict(a=1, b=x) is the literal's call-form twin — a record built
            // through a call so the literal-only scan could be dodged.
            let is_dict = match c.func.as_ref() {
                Expr::Name(n) => n.id.as_str() == "dict",
                Expr::Attribute(a) => a.attr.as_str() == "dict",
                _ => false,
            };
            if is_dict {
                let kwargs = &c.arguments.keywords;
                let scalar_map = kwargs.iter().all(|k| is_scalar_literal(&k.value));
                if kwargs.len() >= 2 && !scalar_map {
                    let keys: Vec<String> = kwargs
                        .iter()
                        .filter_map(|k| k.arg.as_ref().map(|n| n.id.to_string()))
                        .collect();
                    let record = literal_value_record(kwargs.iter().map(|k| &k.value), bindings);
                    found.push(RecordHit {
                        line: line_of(source, c.range().start()),
                        col: col_of(source, c.range().start()),
                        keys,
                        record,
                    });
                }
            }
            // uniform descent: EVERY call's inline arguments are expressions
            // like any other container's
            for a in &c.arguments.args {
                record_literal_scan(a, source, bindings, found);
            }
            for k in &c.arguments.keywords {
                record_literal_scan(&k.value, source, bindings, found);
            }
        }
        _ => {}
    }
}

/// The message's key listing — source order, capped so a generated
/// 40-field record stays one line.
fn display_keys(keys: &[String]) -> String {
    const MAX: usize = 6;
    if keys.len() <= MAX {
        return keys.join(", ");
    }
    format!("{}, … (+{})", keys[..MAX].join(", "), keys.len() - MAX)
}

/// G2: the module's class names that carry a from_dict method — a union
/// `X | dict` blending one of these with the wire dict hides which shape a
/// caller passes; the union message directs the ingestion at entry.
fn module_from_dict_class_names(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        if let Stmt::ClassDef(c) = s {
            let carries = c
                .body
                .iter()
                .any(|m| matches!(m, Stmt::FunctionDef(f) if f.name.to_lowercase().contains("from_dict")));
            if carries {
                out.insert(c.name.to_string());
            }
        }
        match s {
            Stmt::FunctionDef(f) => sq.extend(&f.body),
            _ => push_stmt_children(s, &mut sq),
        }
    }
    out
}

/// G2: the annotation is a union (`A | dict`, `Union[A, dict]`) and a NAME
/// member resolves to a module class that carries from_dict — the union
/// message's {record}. Only unions qualify; a bare class annotation is
/// typed, not a record.
fn union_record_class(e: &Expr, from_dict_classes: &HashSet<String>) -> Option<String> {
    let mut wrapped = Vec::new();
    ann_unwrap(e, &mut wrapped);
    if wrapped.len() < 2 {
        return None;
    }
    wrapped.iter().find_map(|p| match p {
        Expr::Name(n) if from_dict_classes.contains(n.id.as_str()) => Some(n.id.to_string()),
        _ => None,
    })
}

/// F1: does any class in the module carry a from_dict method? Such a class
/// IS the ingestion the wire message recommends — the module's wire-named
/// functions already sit at a parse boundary with a named record, so
/// record-shape does not fire there (a step already taken is not advice).
fn module_has_from_dict_class(body: &[Stmt]) -> bool {
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        if let Stmt::ClassDef(c) = s {
            let carries = c
                .body
                .iter()
                .any(|m| matches!(m, Stmt::FunctionDef(f) if f.name.to_lowercase().contains("from_dict")));
            if carries {
                return true;
            }
        }

        match s {
            Stmt::FunctionDef(f) => sq.extend(&f.body),
            _ => push_stmt_children(s, &mut sq),
        }
    }
    false
}

/// Issue #42, the export mirror of F1: a `to_dict`/`to_json` method on a
/// class that DECLARES fields. The class names the shape the method returns,
/// so neither the `-> dict` return annotation nor a literal rebuilding those
/// fields has advice left to give.
struct ExportEdge {
    /// The method NAME's offset — the signature arm's key (unique per file).
    name_start: ruff_text_size::TextSize,
    /// The method's line span — the literal arm's containment test.
    start_line: usize,
    end_line: usize,
    /// The class's declared field names.
    fields: HashSet<String>,
}

/// Export-edge method names, mirroring the ingestion side's from_dict/from_json
/// pair.
fn is_export_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("to_dict") || n.contains("to_json")
}

/// The module's export edges, in every class (nested ones included). A class
/// DECLARES fields by annotating them in the class body or in __init__, or by
/// assigning self.<x> in a method; a class with no declared field names no
/// shape, so its to_dict is not an edge.
fn module_export_edges(body: &[Stmt], source: &str) -> Vec<ExportEdge> {
    let mut out = Vec::new();
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        if let Stmt::ClassDef(c) = s {
            let mut fields: HashSet<String> = class_field_names(c).into_iter().map(|(n, _)| n).collect();
            fields.extend(class_attr_names(&c.body));
            if !fields.is_empty() {
                for m in &c.body {
                    let Stmt::FunctionDef(f) = m else { continue };
                    if !is_export_name(&f.name) {
                        continue;
                    }
                    out.push(ExportEdge {
                        name_start: f.name.range().start(),
                        start_line: line_of(source, f.range().start()),
                        end_line: line_of(source, f.range().end()),
                        fields: fields.clone(),
                    });
                }
            }
        }
        match s {
            Stmt::FunctionDef(f) => sq.extend(&f.body),
            _ => push_stmt_children(s, &mut sq),
        }
    }
    out
}

/// The collection message's shared continuation: the map is the unnamed
/// shape; the alias answer and the member-function move are the action.
const COLLECTION_MESSAGE_TAIL: &str = "The map itself has no name and no role: call sites pass a bare dict, and nothing says what the collection means. Make a class for the collection, named with a domain noun — an alias to dict names the problem, it does not solve it. Move the operations that take, build, or read this map onto the class as methods, splitting them where the class does not own all the work; which operations belong there is the judgement call.";

/// The dict is a record literal: two or more items, with keys, not a scalar map.
fn is_literal_record(d: &ruff_python_ast::ExprDict, keys: &[String], scalar_map: bool) -> bool {
    d.items.len() >= 2 && !keys.is_empty() && !scalar_map
}

/// The argument is a map of a module class, passed by subscript and not
/// mutated — the shape must be spelled out rather than aliased.
fn is_unspelled_record_map(
    annotation: &Expr,
    function: &StmtFunctionDef,
    record: &str,
    module_classes: &HashSet<String>,
    arg: &str,
) -> bool {
    matches!(annotation, Expr::Subscript(_))
        && module_classes.contains(record)
        && !param_is_mutated(&function.body, arg)
}

/// The record-shape family: signature findings + record dict literals,
/// walking the module in ast.walk BFS order (nested functions included).
pub fn record_shape_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    // F1: the module-level ingestion fact — one scan per module, applied to
    // every wire-named function in it.
    let module_has_ingestion = module_has_from_dict_class(body);
    // G2: the module's from_dict classes — a union-typed parameter blending
    // one of them with the wire dict gets the union message.
    let from_dict_classes = module_from_dict_class_names(body);
    // Issue #46: the module's OWN class names — a map parameter whose value
    // class this module declares is exempt when the function only reads it.
    let module_classes: HashSet<String> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    // Issue #42: the module's export edges — the F1 mirror on the export side.
    let export_edges = module_export_edges(body, source);
    // P1: the module's type bindings — the dict value element's resolution
    // table (module classes, import aliases, known constructors).
    let bindings = module_type_bindings(body);
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                let def_line = line_of(source, f.name.range().start());
                // Issue #42: this function IS an export edge (a to_dict/to_json
                // method whose enclosing class declares fields) — the class
                // names the returned shape, so the return annotation has
                // nothing left to ask for. The literal arm filters per literal,
                // on field coverage.
                let export_edge = export_edges.iter().any(|e| e.name_start == f.name.range().start());
                let mut params: Vec<(&str, Option<&Expr>, usize)> = Vec::new();
                for pwd in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                {
                    // H2: the finding anchors at the ANNOTATED PARAMETER's
                    // own line (its range start), not the def line — a
                    // multi-line signature's params report on their lines.
                    let param_line = line_of(source, pwd.parameter.range().start());
                    params.push((
                        pwd.parameter.name.as_str(),
                        pwd.parameter.annotation.as_deref(),
                        param_line,
                    ));
                }
                if let Some(v) = &f.parameters.vararg {
                    params.push((
                        v.name.as_str(),
                        v.annotation.as_deref(),
                        line_of(source, v.range().start()),
                    ));
                }
                if let Some(k) = &f.parameters.kwarg {
                    params.push((
                        k.name.as_str(),
                        k.annotation.as_deref(),
                        line_of(source, k.range().start()),
                    ));
                }
                // P1 sub-family classification: a dict that ARRIVES at a
                // from_dict/from_json function's parse boundary is built
                // nowhere ad hoc — the A1 "call sites build it ad hoc"
                // premise is false there. Emit the wire message; every other
                // function keeps the internal-ad-hoc text.
                let wire = f.name.to_lowercase().contains("from_dict") || f.name.to_lowercase().contains("from_json");
                // F1: a wire function whose module carries a from_dict class
                // already sits at a typed parse boundary — no record-shape
                // finding at all (the wire message's advice is a step taken).
                if !(wire && module_has_ingestion) {
                    for (arg, ann, param_line) in params {
                        if let Some(a) = ann {
                            if !annotation_is_record(a, &bindings) {
                                continue;
                            }
                            // G2: a union-typed parameter whose record member
                            // is a module class with from_dict — the union
                            // hides which shape a caller passes; direct the
                            // ingestion at entry.
                            let message = if let Some(record) = union_record_class(a, &from_dict_classes) {
                                format!("{arg} accepts the wire dict alongside the {record} class; the union hides which shape a caller passes. Ingest the wire at entry with {record}.from_dict and drop the dict from the union.")
                            } else if wire {
                                format!("{arg} is the wire form of a record; the parse boundary has no type. Give the shape a named record and ingest the wire with its from_dict — call sites can then type against the record instead of the wire.")
                            } else if let Some(record) = map_value_class(a, &bindings) {
                                // H1/P1: the collection is the unnamed shape —
                                // the message names the MAP, never the value
                                // (the annotation already names the value's
                                // class). Issue #46: a READ-ONLY parameter whose
                                // value class this module declares is the one
                                // shape with no missing noun — it comes in named
                                // and nothing is built here — so it is exempt;
                                // a map the function mutates keeps the finding.
                                // The shape must be SPELLED OUT: an alias
                                // (`Sources = dict[str, X]`) or a union
                                // (`dict[str, X] | None`) hides it behind a name,
                                // which is what the rule exists to make explicit
                                // (record_raw_dict_alias_does_not_exempt,
                                // record_optional_named_type_map).
                                if is_unspelled_record_map(a, f, record.as_str(), &module_classes, arg) {
                                    continue;
                                }
                                format!("{arg} is a map whose values are {record}. {COLLECTION_MESSAGE_TAIL}")
                            } else {
                                match signature_record_shape(a, &bindings) {
                                    SignatureRecordShape::FixedTuple => format!("{arg} is a fixed-size tuple; its positions are an unnamed record, so call sites pass positionally and a position change is silent. Type {arg} with a class whose fields name the positions; create that class if none exists."),
                                    SignatureRecordShape::List => format!("{arg} is a list whose items are unnamed records; each item's shape has no name at the call site. Type each item with a class whose fields name the shape; create that class if none exists."),
                                    SignatureRecordShape::Dict => format!("{arg} is a dict; its value is a fixed-shape record. Typed as dict, the shape has no name: call sites build it ad hoc, and field changes go unchecked. Type {arg} with the class that models this shape; create that class if none exists."),
                                }
                            };
                            state.findings.push(Finding {
                                logical_start: None,
                                seam_members: Vec::new(),
                                col: 0,
                                file: state.file.to_string(),
                                line: param_line,
                                function: f.name.to_string(),
                                kind: "record-shape".into(),
                                severity: "fail".into(),
                                message,
                            });
                        }
                    }
                    if let Some(r) = &f.returns {
                        if !export_edge && annotation_is_record(r.as_ref(), &bindings) {
                            // H2: a return finding anchors at the return
                            // annotation's position; the def line only when
                            // that cannot be resolved.
                            let ret_line = {
                                let l = line_of(source, r.range().start());
                                if l == 0 {
                                    def_line
                                } else {
                                    l
                                }
                            };
                            let message = if wire {
                                format!("{fname} returns the wire form of a record; the parse boundary has no type. Give the shape a named record and ingest the wire with its from_dict — call sites can then type against the record instead of the wire.", fname = f.name.as_str())
                            } else if let Some(record) = map_value_class(r.as_ref(), &bindings) {
                                // P1: a return whose dict value element is a
                                // named type renders the collection message —
                                // the map is the unnamed shape, exactly as in
                                // the parameter arm.
                                format!(
                                    "{fname} returns a map whose values are {record}. {COLLECTION_MESSAGE_TAIL}",
                                    fname = f.name.as_str()
                                )
                            } else {
                                match signature_record_shape(r.as_ref(), &bindings) {
                                    SignatureRecordShape::FixedTuple => format!("{fname} returns a fixed-size tuple; its positions are an unnamed record, so a position change is silent at the call site. Type the return with a class whose fields name the positions; create that class if none exists.", fname = f.name.as_str()),
                                    SignatureRecordShape::List => format!("{fname} returns a list whose items are unnamed records; each item's shape has no name at the call site. Type each item with a class whose fields name the shape; create that class if none exists.", fname = f.name.as_str()),
                                    SignatureRecordShape::Dict => format!("{fname} returns a dict that holds the fields of a record; the shape has no name at the call site. Type the return with the class that models the shape; create that class if none exists.", fname = f.name.as_str()),
                                }
                            };
                            state.findings.push(Finding {
                                logical_start: None,
                                seam_members: Vec::new(),
                                col: 0,
                                file: state.file.to_string(),
                                line: ret_line,
                                function: f.name.to_string(),
                                kind: "record-shape".into(),
                                severity: "fail".into(),
                                message,
                            });
                        }
                    }
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    // record dict literals ANYWHERE — shape, not position, defines a record
    // (uniform descent; the record-position carve-out hid call-site wires)
    let mut found: Vec<RecordHit> = Vec::new();
    // descend EVERY node (function bodies included) — each Expr feeds the
    // scan, which recurses internally through containers
    let mut sq: Vec<&Stmt> = body.iter().collect();
    let mut si = 0usize;
    while si < sq.len() {
        // descend ALL statement kinds incl. function/class bodies
        match sq[si] {
            Stmt::FunctionDef(f) => {
                let fd_stmt = Stmt::FunctionDef(f.clone());
                for e in stmt_exprs(&fd_stmt) {
                    record_literal_scan(e, source, &bindings, &mut found);
                }
                for b in &f.body {
                    sq.push(b);
                }
            }
            _ => {
                for e in stmt_exprs(sq[si]) {
                    record_literal_scan(e, source, &bindings, &mut found);
                }
                push_stmt_children(sq[si], &mut sq);
            }
        }
        si += 1;
    }
    // dedupe on (line, col): the dict() call-form and a nested literal can
    // share a range — one finding per record node
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut unique: Vec<RecordHit> = Vec::new();
    for h in found {
        if seen.insert((h.line, h.col)) {
            unique.push(h);
        }
    }
    for h in unique {
        // Issue #42: a literal inside a to_dict/to_json method whose keys are
        // the enclosing class's own declared fields rebuilds a shape the class
        // already names — the export mirror of F1.
        if !h.keys.is_empty()
            && export_edges
                .iter()
                .any(|e| h.line >= e.start_line && h.line <= e.end_line && h.keys.iter().all(|k| e.fields.contains(k)))
        {
            continue;
        }
        let keys = display_keys(&h.keys);
        // P1: a literal whose values ALL name one type is a map of that
        // type — the collection message; any other shape keeps the
        // constant-keys record text.
        let message = match &h.record {
            Some(record) => format!(
                "This dict is a map whose values are {record}. {COLLECTION_MESSAGE_TAIL}"
            ),
            None => format!(
                "This dict has constant keys ({keys}); the keys are fields of one record. Each build site re-creates the keys, so the copies can drift apart. Make a class with these fields and build it once. If the values select behavior (handlers, nodes), keep the dict as a lookup table. — fix: extract-record-class --name <Record>"
            ),
        };
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: state.file.to_string(),
            line: h.line,
            col: h.col,
            function: String::new(),
            kind: "record-shape".into(),
            severity: "fail".into(),
            message,
        });
    }
}
