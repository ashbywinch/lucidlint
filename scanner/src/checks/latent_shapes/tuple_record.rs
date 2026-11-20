//! `tuple-record`: a dict built with same-arity tuple values, then read with
//! constant integer indexes — an anonymous record; make it a class.

use std::collections::HashMap;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::{all_exprs, push_stmt_children, stmt_exprs};

/// The tuple-record rule and the shape readers it needs: the dict's arity, the
/// chain's member name, a constant index, and the reads that count.
/// The positional-read count that makes a tuple a record in waiting.
const POSITIONAL_READS_MIN: usize = 3;

pub struct TupleRecord;

impl TupleRecord {
    /// Report every dict of same-arity tuples read three or more times.
    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        let mut records: HashMap<String, (usize, usize)> = HashMap::new();
        // build sites at ANY scope (build()'s locals are records too) — walk
        // control flow only, not nested defs (a def owns its own bindings)
        let mut stack: Vec<&Stmt> = Vec::new();
        for s in body {
            stack.push(s);
        }
        while let Some(s) = stack.pop() {
            if let Stmt::FunctionDef(f) = s {
                for b in &f.body {
                    stack.push(b);
                }
                continue; // ClassDef descent is push_stmt_children's job
            }
            if let Stmt::Assign(a) = s {
                if a.targets.len() == 1 {
                    if let Some(arity) = Self::dict_tuple_arity(&a.value) {
                        if let Expr::Name(n) = &a.targets[0] {
                            let line = Source::line_of(state.source, n.range().start());
                            records.insert(n.id.to_string(), (arity, line));
                        }
                    }
                }
            }
            push_stmt_children(s, &mut stack);
        }
        if records.is_empty() {
            return;
        }
        // alias resolution: a module-fn param receiving a record as an argument
        let mut module_params: HashMap<String, Vec<String>> = HashMap::new();
        for s in body {
            let Stmt::FunctionDef(f) = s else { continue };
            let mut params = Vec::new();
            for a in &f.parameters.args {
                params.push(a.parameter.name.to_string());
            }
            module_params.insert(f.name.to_string(), params);
        }
        let mut aliases: HashMap<String, String> = HashMap::new();
        {
            let mut stack: Vec<&Stmt> = Vec::new();
            for s in body {
                stack.push(s);
            }
            while let Some(s) = stack.pop() {
                if let Stmt::FunctionDef(f) = s {
                    for b in &f.body {
                        stack.push(b);
                    }
                    continue;
                }
                for e in stmt_exprs(s) {
                    let Expr::Call(c) = e else { continue };
                    let Expr::Name(fn_name) = c.func.as_ref() else { continue };
                    let Some(params) = module_params.get(fn_name.id.as_str()) else {
                        continue;
                    };
                    for (i, arg) in c.arguments.args.iter().enumerate() {
                        if let (Some(p), Expr::Name(n)) = (params.get(i), arg) {
                            if records.contains_key(n.id.as_str()) {
                                aliases.insert(p.clone(), n.id.to_string());
                            }
                        }
                    }
                }
                push_stmt_children(s, &mut stack);
            }
        }
        // count reads across every function body
        let mut counts: HashMap<String, usize> = HashMap::new();
        let arities: HashMap<String, usize> = records.iter().map(|(n, (a, _))| (n.clone(), *a)).collect();
        for s in body {
            match s {
                Stmt::FunctionDef(f) => Self::count_reads(&f.body, &arities, &aliases, &mut counts),
                Stmt::ClassDef(c) => {
                    for m in &c.body {
                        if let Stmt::FunctionDef(f) = m {
                            Self::count_reads(&f.body, &arities, &aliases, &mut counts);
                        }
                    }
                }
                _ => {}
            }
        }
        for (name, (arity, line)) in &records {
            let reads = counts.get(name).copied().unwrap_or(0);
            if reads >= POSITIONAL_READS_MIN {
                state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                file: state.file.to_string(),
                                line: *line,
                                function: String::new(),
                                kind: "tuple-record".into(),
                                severity: "fail".into(),
                                message: format!(
                                    "the values of '{name}' are {arity}-tuples read {reads} times by constant index — an anonymous record; make it a class named with a domain noun (apply: lucidlint fix --kind tuple-record --name <N>) — fix: tuple-record"
                                ), });
            }
        }
    }

    /// A dict whose values are same-arity tuples (arity >= 2) — the anonymous
    /// record shape. None for mixed/unpacked dicts.
    fn dict_tuple_arity(e: &Expr) -> Option<usize> {
        match e {
            Expr::Dict(d) => {
                let mut arity = None;
                for item in &d.items {
                    item.key.as_ref()?; // **unpacking — cannot verify every value
                    match &item.value {
                        Expr::Tuple(t) => {
                            let n = t.elts.len();
                            if n < 2 {
                                return None;
                            }
                            if arity.is_some_and(|a| a != n) {
                                return None;
                            }
                            arity = Some(n);
                        }
                        _ => return None,
                    }
                }
                arity
            }
            Expr::DictComp(dc) => match dc.value.as_ref() {
                Expr::Tuple(t) => (t.elts.len() >= 2).then_some(t.elts.len()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The accessed member of a chain: self.em -> em, cfg.table -> table,
    /// em -> em. The closures receiver peel needs the BASE name; the tuple-record
    /// reads need the member.
    fn attr_chain_last_name(e: &Expr) -> Option<&str> {
        match e {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Attribute(a) => Some(a.attr.as_str()),
            _ => None,
        }
    }

    /// A literal non-negative integer index (1 in `em[cid][1]`).
    fn const_int_index(e: &Expr) -> Option<usize> {
        match e {
            Expr::NumberLiteral(n) => match &n.value {
                ruff_python_ast::Number::Int(i) => i.as_u8().map(|v| v as usize),
                _ => None,
            },
            _ => None,
        }
    }

    /// Count the reads of each record across one function body.
    fn count_reads(
        body: &[Stmt],
        records: &HashMap<String, usize>,
        aliases: &HashMap<String, String>,
        counts: &mut HashMap<String, usize>,
    ) {
        fn bump(
            base: &str,
            records: &HashMap<String, usize>,
            aliases: &HashMap<String, String>,
            counts: &mut HashMap<String, usize>,
        ) {
            let base = aliases.get(base).map(String::as_str).unwrap_or(base);
            if records.contains_key(base) {
                *counts.entry(base.to_string()).or_insert(0) += 1;
            }
        }
        let mut stack: Vec<&Stmt> = Vec::new();
        for s in body {
            stack.push(s);
        }
        while let Some(s) = stack.pop() {
            if let Stmt::FunctionDef(f) = s {
                for b in &f.body {
                    stack.push(b);
                }
                continue; // ClassDef descent is push_stmt_children's job
            }
            // pattern 1: NAME[KEY][K] with a constant index — including the
            // reads nested inside comprehensions
            let mut exprs: Vec<&Expr> = Vec::new();
            for e in stmt_exprs(s) {
                all_exprs(e, &mut exprs);
            }
            for e in exprs {
                if let Expr::Subscript(outer) = e {
                    if let Expr::Subscript(inner) = outer.value.as_ref() {
                        if let Some(base) = Self::attr_chain_last_name(&inner.value) {
                            if Self::const_int_index(&outer.slice).is_some() {
                                bump(base, records, aliases, counts);
                            }
                        }
                    }
                }
            }
            // pattern 2: `p, nm = NAME[key]` — a tuple of plain names unpacking
            // a record value
            if let Stmt::Assign(a) = s {
                if let Some(Expr::Tuple(t)) = a.targets.first() {
                    if t.elts.iter().all(|el| matches!(el, Expr::Name(_))) {
                        if let Expr::Subscript(sub) = a.value.as_ref() {
                            if let Some(b) = Self::attr_chain_last_name(&sub.value) {
                                bump(b, records, aliases, counts);
                            }
                        }
                    }
                }
            }
            // pattern 3: `for k, (a, b) in NAME.items()` — a nested tuple target
            if let Stmt::For(f) = s {
                if let Expr::Attribute(attr) = f.iter.as_ref() {
                    if attr.attr.as_str() == "items" {
                        if let Some(base) = Self::attr_chain_last_name(&attr.value) {
                            let mut has_nested = false;
                            let mut tstack: Vec<&Expr> = Vec::new();
                            tstack.push(&f.target);
                            while let Some(t) = tstack.pop() {
                                if let Expr::Tuple(tp) = t {
                                    if tp.elts.iter().any(|el| matches!(el, Expr::Tuple(_))) {
                                        has_nested = true;
                                    }
                                    for el in &tp.elts {
                                        tstack.push(el);
                                    }
                                }
                            }
                            if has_nested {
                                bump(base, records, aliases, counts);
                            }
                        }
                    }
                }
            }
            push_stmt_children(s, &mut stack);
        }
    }
}
