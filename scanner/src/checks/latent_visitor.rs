//! The latent-visitor refactoring (advisory): a dispatch chain inside an
//! enumerated visit is a visitor waiting to happen — the message names the
//! refactoring so the reader can hand-apply it.
// lucidlint: ignore-file complexity the dispatch walker is ONE exhaustive match over
// the AST's surface — the arm count is the language's shape, not branching

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::{source::line_of, Finding, ScanState};

/// Is the guarded branch of a None/empty check a FAIL-FAST GUARD — it raises
/// or returns an error shape (an HTTPException, a *Response, a dict with an
/// "error" key)? A guard is not a special-case candidate: the absence IS an
/// error, so no object can replace the repeated handling — the "give the
/// absent case an object" refactoring would mask it (review log §2.6: person
/// is None -> KeyError, mine is None -> 401).
pub(super) fn branch_guards(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Raise(_) => return true,
            Stmt::Return(r) => {
                if let Some(e) = r.value.as_deref() {
                    if return_is_error(e) {
                        return true;
                    }
                }
            }
            Stmt::If(i) => {
                if branch_guards(&i.body) || i.elif_else_clauses.iter().any(|cl| branch_guards(&cl.body)) {
                    return true;
                }
            }
            Stmt::For(f) => {
                if branch_guards(&f.body) || f.orelse.iter().any(|s| branch_guards(std::slice::from_ref(s))) {
                    return true;
                }
            }
            Stmt::While(w) => {
                if branch_guards(&w.body) || w.orelse.iter().any(|s| branch_guards(std::slice::from_ref(s))) {
                    return true;
                }
            }
            Stmt::With(w) => {
                if branch_guards(&w.body) {
                    return true;
                }
            }
            Stmt::Try(t) => {
                if branch_guards(&t.body) {
                    return true;
                }
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    if branch_guards(&eh.body) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn return_is_error(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => {
                let name = n.id.as_str();
                name == "HTTPException" || name.ends_with("Response")
            }
            Expr::Attribute(a) => {
                let attr = a.attr.as_str();
                attr == "HTTPException" || attr.ends_with("Response")
            }
            _ => false,
        },
        Expr::Dict(d) => d
            .items
            .iter()
            .any(|item| matches!(&item.key, Some(Expr::StringLiteral(s)) if s.value.to_str() == "error")),
        _ => false,
    }
}
/// Latent Visitor: >= 2 functions dispatch over the SAME element family
/// (isinstance / type()== / __class__ comparisons) — the signal that the
/// OPERATIONS vary independently of the elements (GoF's visitor criterion).
/// Detection-only: the message names the refactoring; the fix (a Visitor
/// class + accept methods on every element) is structural.
///
/// The anti-thrash contract: every chain this rule claims is recorded in
/// `claimed_dispatch` so conditional-polymorphism skips it — one ruling per
/// chain. Single-operation dispatch stays polymorphism's territory.
pub fn latent_visitor_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    // per family: (dispatch-site count, first-site line)
    let mut families: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    let mut site_lines: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    fn walk(
        stmts: &[Stmt],
        source: &str,
        families: &mut std::collections::HashMap<String, (usize, usize)>,
        site_lines: &mut std::collections::HashMap<usize, String>,
    ) {
        for s in stmts {
            if let Stmt::If(i) = s {
                let arms: Vec<&Expr> = std::iter::once(i.test.as_ref())
                    .chain(i.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()))
                    .collect();
                if arms.len() >= 2 {
                    // the chain's FAMILY is the SET of types it dispatches
                    // on (sorted, deduped) — the visitor signal is the
                    // MULTI-SITE (>= 2 operations over the same type set),
                    // not the arm count; keying by the value's name would
                    // merge chains that share a variable but test different
                    // types (and could suppress a conditional-polymorphism
                    // ruling on one of them)
                    let type_keys: Vec<Option<String>> =
                        arms.iter().map(|t| dispatch_arm(t).map(|(tk, _)| tk)).collect();
                    if type_keys.iter().all(Option::is_some) {
                        let mut fam: Vec<String> = type_keys.into_iter().flatten().collect();
                        fam.sort_unstable();
                        fam.dedup();
                        let chain_family = fam.join("|");
                        let line = line_of(source, s.range().start());
                        let e = families.entry(chain_family.clone()).or_insert((0, line));
                        e.0 += 1;
                        site_lines.insert(line, chain_family);
                    }
                }
                walk(&i.body, source, families, site_lines);
                for cl in &i.elif_else_clauses {
                    walk(&cl.body, source, families, site_lines);
                }
            } else {
                match s {
                    Stmt::FunctionDef(f) => walk(&f.body, source, families, site_lines),
                    Stmt::ClassDef(cd) => walk(&cd.body, source, families, site_lines),
                    Stmt::For(f) => walk(&f.body, source, families, site_lines),
                    Stmt::While(w) => walk(&w.body, source, families, site_lines),
                    _ => {}
                }
            }
        }
    }
    walk(body, source, &mut families, &mut site_lines);
    let qualifying: std::collections::HashSet<String> = families
        .iter()
        .filter(|(_, (n, _))| *n >= 2)
        .map(|(f, _)| f.clone())
        .collect();
    for (line, family) in &site_lines {
        if qualifying.contains(family) {
            state.claimed_dispatch.insert(*line);
        }
    }
    let mut v: Vec<(&String, &(usize, usize))> = families.iter().collect();
    v.sort_by_key(|(_, (_, l))| *l);
    for (family, (n, line)) in v {
        if *n >= 2 {
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: state.file.to_string(),
                            line: *line,
                            function: String::new(),
                            kind: "latent-visitor".into(),
                            severity: "warn".into(),
                            message: format!(
                                "{n} operations branch on the same object type ('{family}') — Replace Conditional with Visitor: give each type a visit_<Type> method so the branches disappear"
                            ), });
        }
    }
}

/// The (family, dispatched type) of one chain-arm test — the discriminated
/// value and the type it dispatches on. Value comparisons (`x == 1`) return
/// None: they are not type-tag dispatch.
fn dispatch_arm(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Call(c) => {
            if let Expr::Name(n) = c.func.as_ref() {
                if n.id.as_str() == "isinstance" {
                    let args = &c.arguments.args;
                    if args.len() >= 2 {
                        let Expr::Name(f) = &args[0] else {
                            return None;
                        };
                        // the FAMILY is the dispatched TYPE — two functions
                        // that both use `x` but test different types are
                        // different element families; keying by the value's
                        // name merged them and could suppress a legitimate
                        // conditional-polymorphism ruling on one of them
                        match &args[1] {
                            Expr::Name(t) => return Some((t.id.to_string(), f.id.to_string())),
                            Expr::Tuple(t) => {
                                let types: Vec<String> = t
                                    .elts
                                    .iter()
                                    .filter_map(|e| {
                                        if let Expr::Name(n) = e {
                                            Some(n.id.to_string())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                if types.len() == 1 {
                                    return Some((types[0].clone(), f.id.to_string()));
                                }
                                return Some((types.join("|"), f.id.to_string()));
                            }
                            _ => return None,
                        }
                    }
                }
            }
            None
        }
        Expr::Compare(c) => {
            // type(x) == A / x.__class__ is A / x is A
            let family = match c.left.as_ref() {
                Expr::Name(n) => n.id.to_string(),
                Expr::Call(tc) => {
                    if let Expr::Name(f) = tc.func.as_ref() {
                        if f.id.as_str() == "type" && tc.arguments.args.len() == 1 {
                            if let Expr::Name(x) = &tc.arguments.args[0] {
                                x.id.to_string()
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            };
            if c.ops.len() == 1 && c.comparators.len() == 1 {
                if let Expr::Name(t) = &c.comparators[0] {
                    // (type, value) — the family is the compared TYPE
                    return Some((t.id.to_string(), family));
                }
            }
            None
        }
        _ => None,
    }
}
