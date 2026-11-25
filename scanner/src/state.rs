//! The scan state and its AST visitor: the per-file facts every rule reads.

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::checks;
use crate::checks::*;
use crate::finding::{Finding, FnCc, FnScope};
use crate::python_shape::FnShape;
use crate::source::Source;
use crate::ScanState;

/// The parent expr's KIND for the magic-number position rule — the variant
/// discriminant, never the node: holding a cloned `Expr` per visited node
/// deep-cloned every subtree (the scan's biggest allocation; a keystroke on
/// a 2000-line file cloned the whole AST twice over).
#[derive(Copy, Clone, PartialEq)]
pub(crate) enum ParentExprKind {
    BinOp,
    Compare,
    UnaryOp,
    Subscript,
    Call,
    Other,
}

impl ParentExprKind {
    /// The variant discriminant for an expression — never the node itself.
    pub(crate) fn of(e: &Expr) -> ParentExprKind {
        match e {
            Expr::BinOp(_) => ParentExprKind::BinOp,
            Expr::Compare(_) => ParentExprKind::Compare,
            Expr::UnaryOp(_) => ParentExprKind::UnaryOp,
            Expr::Subscript(_) => ParentExprKind::Subscript,
            Expr::Call(_) => ParentExprKind::Call,
            _ => ParentExprKind::Other,
        }
    }
}

pub(crate) enum ParentEntry<'a> {
    Expr(ParentExprKind),
    /// A positional argument of a call — the callee's name (P2: the
    /// unit-constructor exemption).
    CallArg {
        callee: &'a str,
    },
    /// A keyword argument of a call — callee + the keyword's name (P2: a
    /// unit-naming keyword — timedelta(minutes=20) — states the unit).
    CallKw {
        callee: &'a str,
        kw: &'a str,
    },
    Stmt,
}

impl<'a> SourceOrderVisitor<'a> for ScanState<'a> {
    // lucidlint: ignore large-function the ruff visitor dispatches statement kinds in CPython's visit order
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        // module scope = no open function scope and no class nesting (the
        // FunctionDef arm walks bodies manually, so parent_stack is not the
        // reliable signal here)
        let module_level = self.fn_stack.is_empty() && self.in_class == 0;
        match stmt {
            Stmt::FunctionDef(f) => {
                let module_level = self.fn_stack.is_empty() && self.in_class == 0;
                checks::ExpressionOccurrence::collect(self, f);
                if module_level {
                    checks::PathExpression::collect(self, f);
                }
                let was_fn = self.current_fn.take();
                self.current_fn = Some((f.name.to_string(), Source::line_of(self.source, f.range.start())));
                self.fn_spans.push((
                    f.name.to_string(),
                    Source::line_of(self.source, f.range.start()),
                    Source::line_of(self.source, f.range.end()),
                ));

                self.fn_stack.push(FnScope {
                    returned: FunctionFacts::returned_names(&f.body),
                });
                self.unreachable_check(&f.body);
                shadow_findings(self, stmt);
                let source = self.source;
                LongParamList::findings(self, f, source);
                // signature exprs feed the reference scan (ast.walk covers
                // decorators, parameter annotations/defaults, and returns)
                for d in &f.decorator_list {
                    self.visit_expr(&d.expression);
                }
                for pwd in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .chain(&f.parameters.kwonlyargs)
                {
                    if let Some(a) = &pwd.parameter.annotation {
                        self.visit_expr(a.as_ref());
                    }
                    if let Some(d) = &pwd.default {
                        self.visit_expr(d.as_ref());
                    }
                }
                for p in [&f.parameters.vararg, &f.parameters.kwarg].into_iter().flatten() {
                    if let Some(a) = &p.annotation {
                        self.visit_expr(a.as_ref());
                    }
                }
                if let Some(r) = &f.returns {
                    self.visit_expr(r.as_ref());
                }
                for s in &f.body {
                    self.visit_stmt(s);
                }
                // the push/pop invariant is per-function; a missing scope is a
                // walker bug, not a crash — bail out of this function's checks
                let Some(_scope) = self.fn_stack.pop() else {
                    return;
                };
                // The radon-equivalent CC comes from the radonc crate (its rules
                // match radon 6.0.1 exactly; the visitor still tracks decisions
                // for the closures/latent-class gate).
                let cc = radonc::function_cc(f) as u32;
                // repo-wide collections: module-level defs + duplicate
                // candidates (the def line is the NAME's line — ruff's
                // FunctionDef range starts at the decorator)
                let def_line = Source::line_of(self.source, f.name.range().start());
                if module_level && !self.is_test {
                    self.defs.push((f.name.to_string(), def_line));
                    if !f.decorator_list.is_empty() {
                        self.decorated.insert(f.name.to_string());
                    }
                }
                let span = (Source::line_of(self.source, f.range().end())
                    - Source::line_of(self.source, f.range().start())) as u32;
                // the Python gate reads cc from radon's fn_map, which only
                // holds module-level functions — methods/nested get cc = 0
                let gate_cc = if module_level { cc } else { 0 };
                Closures::closure_findings(self, stmt, gate_cc, span);
                // a method that never touches its receiver does not belong in
                // the class (the inverse of record-shape/strewing)
                if self.in_class > 0 {
                    DetachedMethod::findings(self, f, source);
                }
                if module_level {
                    // the def line (name range), not the decorator — radon's
                    // fn.lineno; the parity test's decorated-line offset
                    // normalization existed because of this difference
                    let (shape, shape_detail) = FnShape::python_fn_shape(&f.body);
                    self.cc.push(FnCc {
                        file: self.file.to_string(),
                        function: f.name.to_string(),
                        line: def_line,
                        cc,
                        shape,
                        shape_detail,
                    });
                }
                self.current_fn = was_fn;
                return;
            }
            Stmt::ClassDef(cls) => {
                self.in_class += 1;
                // class bodies: no decisions, but walked (imports/exprs inside
                // still get visited with their function attribution)
                if module_level && !self.is_test {
                    let def_line = Source::line_of(self.source, cls.name.range().start());
                    self.class_defs.push((cls.name.to_string(), def_line));
                }
                walk_stmt(self, stmt);
                self.in_class -= 1;
                return;
            }
            Stmt::Expr(e) => self.noop_check(&e.value),
            Stmt::Import(imp) => {
                self.import_checks(stmt);
                for a in &imp.names {
                    self.refs.insert(a.name.to_string());
                }
            }
            Stmt::ImportFrom(imp) => {
                self.import_checks(stmt);
                for a in &imp.names {
                    self.refs.insert(a.name.to_string());
                }
            }
            Stmt::For(_) | Stmt::With(_) | Stmt::Try(_) => {
                if matches!(stmt, Stmt::Try(_)) {
                    Except::findings(self, stmt);
                }
                shadow_findings(self, stmt);
            }
            Stmt::Global(_) => {
                let module_level = false; // globals inside a function
                GlobalState::global_state_findings(self, stmt, module_level);
            }
            Stmt::Assign(_) => {
                GlobalState::global_state_findings(self, stmt, module_level);
                shadow_findings(self, stmt);
            }
            Stmt::AugAssign(_) | Stmt::AnnAssign(_) | Stmt::Delete(_) => {
                GlobalState::global_state_findings(self, stmt, module_level);
            }
            _ => {}
        }
        // CC (cyclomatic complexity) comes from the radonc crate — the scan
        // walk itself no longer counts decisions.
        self.parent_stack.push(ParentEntry::Stmt);
        walk_stmt(self, stmt);
        self.parent_stack.pop();
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                self.refs.insert(n.id.to_string());
            }
            Expr::Attribute(a) => {
                // attribute names count as references (obj.set_x(v) uses set_x)
                self.refs.insert(a.attr.to_string());
                let attr = a.attr.as_str();
                if attr.starts_with('_') && !Self::is_dunder(attr) {
                    if let Some((root, receiver)) = Self::dotted_path(&a.value) {
                        let line = Source::line_of(self.source, a.range().start());
                        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                        self.private_reaches
                            .push((root, format!("{receiver}.{attr}"), line, fn_name));
                    }
                }
            }
            Expr::StringLiteral(s) if !self.is_test => {
                self.strings.push(s.value.to_str().to_string());
            }
            _ => {}
        }
        // Calls are walked manually so keyword values get a Keyword parent —
        // the generic walk would hand them the Call as parent, and the magic
        // position rule must match Python's (status_code=403 is NOT a finding).
        if let Expr::Call(call) = expr {
            let source = self.source;
            BooleanArg::findings(self, call, source);
            PositionalLiterals::findings(self, call, source);
            // debug-artifact (Python half): breakpoint() left in production —
            // the dbg!/unwrap analog (Rust half lives in rustscan.rs)
            if !self.is_test
                && matches!(
                    call.func.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "breakpoint"
                )
            {
                let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                self.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    file: self.file.to_string(),
                    line: Source::line_of(source, call.range().start()),
                    col: 0,
                    function: fn_name,
                    kind: "debug-artifact".into(),
                    severity: "fail".into(),
                    message: "breakpoint() left in production code — remove it".into(),
                });
            }
            let callee: &'a str = match call.func.as_ref() {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                _ => "",
            };
            self.parent_stack.push(ParentEntry::Expr(ParentExprKind::of(expr)));
            self.visit_expr(&call.func);
            for arg in &call.arguments.args {
                self.parent_stack.push(ParentEntry::CallArg { callee });
                self.visit_expr(arg);
                self.parent_stack.pop();
            }
            for kw in &call.arguments.keywords {
                let name: &'a str = kw.arg.as_ref().map(|n| n.id.as_str()).unwrap_or("");
                self.parent_stack.push(ParentEntry::CallKw { callee, kw: name });
                self.visit_expr(&kw.value);
                self.parent_stack.pop();
            }
            self.parent_stack.pop();
            return;
        }
        // magic numbers (a warn finding — distinct from the CC that radonc owns).
        // Magic fires ANYWHERE expressions are walked — class bodies and
        // methods included (radon/reference parity): the fixer's constant
        // becomes a CLASS ATTRIBUTE of the enclosing class, so a class-body
        // finding is the rule's primary case (third-pass 4).
        if let Expr::NumberLiteral(n) = expr {
            self.magic_check(n);
        }
        self.parent_stack.push(ParentEntry::Expr(ParentExprKind::of(expr)));
        walk_expr(self, expr);
        self.parent_stack.pop();
    }
}

impl<'a> ScanState<'a> {
    /// Magic numbers: int/float literals outside (0, 1, 2) whose parent is an
    /// operation — mirrors the Python implementation's position rule.
    /// Exempt (B8): octal/hex/binary spellings (0o600 is intent-bearing, and
    /// the old code rendered it as "384"), plain subscript indexes, len()
    /// length guards, module-level named-constant RHS values, and >= 3
    /// same-kind data-table siblings in one collection literal.
    fn magic_check(&mut self, n: &ruff_python_ast::ExprNumberLiteral) {
        // B8: an 0x/0o/0b spelling states its meaning in the literal itself
        let text = &self.source[n.range()];
        if text.len() >= 2 {
            let prefix = &text[..2];
            if checks::MagicNumber::is_radix_prefix(prefix) {
                return;
            }
        }
        let value = match &n.value {
            ruff_python_ast::Number::Int(i) => i.to_string(),
            ruff_python_ast::Number::Float(f) => f.to_string(),
            ruff_python_ast::Number::Complex { .. } => return,
        };
        if matches!(value.as_str(), "0" | "1" | "2") {
            return;
        }
        let parent_is_op = matches!(
            self.parent_stack.last(),
            Some(ParentEntry::Expr(k)) if matches!(
                k,
                ParentExprKind::BinOp
                    | ParentExprKind::Compare
                    | ParentExprKind::UnaryOp
                    | ParentExprKind::Call
            )
        ) || matches!(self.parent_stack.last(), Some(ParentEntry::CallArg { .. }));
        if !parent_is_op {
            return; // a bare subscript index is a position code, not domain magic
        }
        let at = n.range().start().to_usize();
        if self.magic_table_exempts.contains(&at)
            || self.magic_const_rhs_exempts.contains(&at)
            || self.magic_len_guard_exempts.contains(&at)
            || self.magic_unit_named_exempts.contains(&at)
        {
            return;
        }
        // P2: the nearest enclosing call's callee is a unit constructor
        // (Quantity, timedelta, TimeDelta, Distance) or its keyword names a
        // unit (timedelta(minutes=20)) — the unit is stated at the site, so
        // the literal is named there.
        let nearest_call = self.parent_stack.iter().rev().find_map(|e| match e {
            ParentEntry::CallArg { callee } => Some((*callee, "")),
            ParentEntry::CallKw { callee, kw } => Some((*callee, *kw)),
            _ => None,
        });
        if let Some((callee, kw)) = nearest_call {
            let unit_ctor = matches!(callee, "Quantity" | "timedelta" | "TimeDelta" | "Distance");
            let unit_kw = !kw.is_empty() && checks::NamingUnits::name_states_unit(kw);
            if unit_ctor || unit_kw {
                return;
            }
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        let in_fn = if fn_name.is_empty() {
            String::new()
        } else {
            format!(" in {fn_name}")
        };
        // P2: both mechanisms are named as glossed examples; the family is
        // the reader's judgment, so the same message renders at every firing
        // site and the suggestion NEVER names a unit or claims which applies.
        let message = format!("magic number {value}{in_fn} — nothing states what it means or why this magnitude. Write it as a named constant where the computation uses it, and state what it means and why this size in the name or in one comment beside it. If the value carries units, express it in a type that understands them — Python's timedelta for durations, or a Quantity from the pint units library for physical measures. Collection-literal data tables (>= 3 same-kind numeric siblings) are exempt. — fix: magic-number --fix-name <CONST>");
        self.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: self.file.to_string(),
            line: Source::line_of(self.source, n.range.start()),
            col: Source::col_of(self.source, n.range.start()),
            function: fn_name,
            kind: "magic-number".into(),
            severity: "warn".into(),
            message,
        });
    }

    /// No-op statements: expression statements that discard their value.
    fn noop_check(&mut self, v: &'a Expr) {
        let harmless = matches!(
            v,
            Expr::Call(_)
                | Expr::Await(_)
                | Expr::Yield(_)
                | Expr::YieldFrom(_)
                | Expr::StringLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NoneLiteral(_)
                | Expr::EllipsisLiteral(_)
                | Expr::Lambda(_)
                | Expr::Named(_)
        );
        if !harmless {
            let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
            self.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: self.file.to_string(),
                line: Source::line_of(self.source, v.range().start()),
                col: 0,
                function: fn_name,
                kind: "noop-statement".into(),
                severity: "fail".into(),
                message: "expression statement discards its value — dead statement — fix: noop-statement".into(),
            });
        }
    }

    /// Inline imports (imports inside any function) and private-symbol
    /// imports (`from pkg import _x`, `from pkg._sub import x`,
    /// `import pkg._internal`).
    fn import_checks(&mut self, stmt: &'a Stmt) {
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        if !self.fn_stack.is_empty() {
            let line = Source::stmt_line(self.source, stmt);
            self.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: self.file.to_string(),
                line,
                function: fn_name.clone(),
                kind: "inline-import".into(),
                severity: "fail".into(),
                message: format!("import inside function body at line {line} — move it to module top"),
            });
        }
        match stmt {
            Stmt::ImportFrom(im) => {
                if im.module.as_ref().map(|m| m.as_str()) == Some("__future__") {
                    return;
                }
                // a relative import is the file's own package by definition
                let source = if im.level > 0 {
                    ""
                } else {
                    im.module.as_ref().map(|m| m.as_str()).unwrap_or("")
                };
                let private_module = im
                    .module
                    .as_ref()
                    .map(|m| m.as_str().split('.').any(|seg| seg.starts_with('_')))
                    .unwrap_or(false);
                for alias in &im.names {
                    let name = alias.name.as_str();
                    if name != "*" {
                        let bound = alias.asname.as_ref().map(|a| a.as_str()).unwrap_or(name);
                        self.import_sources.insert(bound.to_string(), source.to_string());
                    }
                    if name.starts_with('_') || private_module {
                        let target = if name.starts_with('_') {
                            name.to_string()
                        } else {
                            format!("{}.{}", im.module.as_ref().map(|m| m.as_str()).unwrap_or(""), name)
                        };
                        self.findings.push(Finding {
                            logical_start: None,
                            seam_members: Vec::new(),
                            file: self.file.to_string(),
                            line: Source::stmt_line(self.source, stmt),
                            col: 0,
                            function: fn_name.clone(),
                            kind: "private-code-leakage".into(),
                            severity: "fail".into(),
                            message: format!(
                                concat!(
                                    "imports private symbol '{target}' from another module — the name is an implementation detail, ",
                                    "and depending on it couples this code to a choice the owner may change. ",
                                    "Renaming it public is not the fix: that turns a detail into a promise. ",
                                    "Call the public API that expresses the need, or move the behaviour into the owning module. ",
                                    "Make it public only when it is part of the module's contract."
                                ),
                                target = target
                            ),
                        });
                    }
                }
            }
            Stmt::Import(im) => {
                for alias in &im.names {
                    let name = alias.name.as_str();
                    let bound = alias
                        .asname
                        .as_ref()
                        .map(|a| a.as_str())
                        .unwrap_or_else(|| name.split('.').next().unwrap_or(name));
                    self.import_sources.insert(bound.to_string(), name.to_string());
                    if name.split('.').any(|seg| seg.starts_with('_')) {
                        self.findings.push(Finding {
                            logical_start: None,
                            seam_members: Vec::new(),
                            file: self.file.to_string(),
                            line: Source::stmt_line(self.source, stmt),
                            col: 0,
                            function: fn_name.clone(),
                            kind: "private-code-leakage".into(),
                            severity: "fail".into(),
                            message: format!(
                                concat!(
                                    "imports private module path '{name}' — the layout is an implementation detail, ",
                                    "and depending on it couples this code to a structure the owner may change. ",
                                    "Dropping the underscore is not the fix: that turns a private module into a promise. ",
                                    "Use the package's public surface, or move the behaviour into the package. ",
                                    "Make it public only when it is part of the package's contract."
                                ),
                                name = name
                            ),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Statements after an unconditional return/raise/continue/break are dead
    /// code — one finding per statement list, exactly like the Python tool.
    fn unreachable_check(&mut self, body: &'a [Stmt]) {
        let mut lists: Vec<&'a [Stmt]> = vec![body];
        for s in body {
            FnShape::collect_stmt_lists(s, &mut lists);
        }
        let fn_name = self.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        for list in lists {
            for (i, stmt) in list.iter().enumerate() {
                let exits = matches!(
                    stmt,
                    Stmt::Return(_) | Stmt::Raise(_) | Stmt::Continue(_) | Stmt::Break(_)
                );
                if exits {
                    if let Some(dead) = list.get(i + 1) {
                        let line = Source::stmt_line(self.source, dead);
                        self.findings.push(Finding {
                            logical_start: None,
                            seam_members: Vec::new(),
                            col: 0,
                            file: self.file.to_string(),
                            line,
                            function: fn_name.clone(),
                            kind: "unreachable".into(),
                            severity: "fail".into(),
                            message: format!(
                                "unreachable statement at line {line} — dead code is deleted — fix: unreachable"
                            ),
                        });
                    }
                    break;
                }
            }
        }
    }
}

impl ScanState<'_> {
    /// The module-level post-passes — structural families + the advisory
    /// refactorings — kept out of Self::scan_source_impl (which was crossing the
    /// large-function line).
    pub(crate) fn module_post_passes(&mut self, body: &[Stmt], name: &str, source: &str, repo_wide: bool) {
        class_module_findings(self, body, name);
        vague_name_findings(self, body);
        strewing_findings(self, body);
        MisplacedMethod::findings(self, body);
        TupleRecord::findings(self, body);
        WideTuple::findings(self, body);
        AssemblyClass::findings(self, body);
        LatentState::data_clump(self, body);
        LatentState::feature_envy(self, body);
        LatentState::collect_self_assigns(self, body);
        StaticHusk::findings(self, body);
        delegating_husk_findings(self, body);
        process_class_findings(self, body);
        ClosureCluster::findings(self, body, source);
        LatentState::duplicate_field(self, body);
        RecordShape::findings(self, body, source);
        Partition::findings(self, body, source);
        // review-log rules: shadowing hazards + duplicated work (all per-file)
        duplicate_def_findings(self, body);
        Docstring::findings(self, body);
        duplicate_block_findings(self, body);
        // advisory refactorings (warn): detection-only, the fix names the Fowler
        // refactoring for the agent (or a future fix)
        guard_clause_findings(self, body, source);
        latent_visitor_findings(self, body, source); // claims chains first — polymorphism skips them
        conditional_polymorphism_findings(self, body, source);
        SpecialCase::findings(self, body, source);
        Scattered::collect_scattered_instances(self, body);
        UnnamedCondition::findings(self, body, source);
        UnnamedClassification::findings(self, body, source);
        MiddleMan::collect(self, body, source, repo_wide);
        Unused::collect_setters(self, body);
        LoopFamily::findings(self, body, source);
        DuplicateModule::collect(self, body, name);
        Forwarding::collect(self, body);
        // the reaches are drained here, in the caller, so the findings pass
        // cannot silently read an empty cache if it were ever called twice
        let reaches = std::mem::take(&mut self.private_reaches);
        self.private_code_leakage_findings(reaches);
    }

    /// The receiver chain of a `name.attr.attr` expression: the root name and
    /// the dotted path as written. `None` when the receiver is not a name chain
    /// (a call result, a subscript), which the leakage rule leaves alone.
    fn dotted_path(expr: &Expr) -> Option<(String, String)> {
        match expr {
            Expr::Name(n) => Some((n.id.to_string(), n.id.to_string())),
            Expr::Attribute(a) => {
                let (root, mut path) = Self::dotted_path(&a.value)?;
                path.push('.');
                path.push_str(a.attr.as_str());
                Some((root, path))
            }
            _ => None,
        }
    }

    /// Protocol names (`__name__`, `__enter__`) are public by convention; only
    /// a single-underscore or name-mangled name is private.
    fn is_dunder(name: &str) -> bool {
        name.starts_with("__") && name.ends_with("__")
    }

    /// The dotted package of a file path: `pkg/sub/__init__.py` -> `pkg.sub`.
    fn package_of(file: &str) -> String {
        file.rsplit_once('/')
            .map(|(dir, _)| dir.replace('/', "."))
            .unwrap_or_default()
    }

    /// True when `source` is the package itself or a module inside it. A
    /// relative import (empty source) is the file's own package by definition.
    fn same_package(package: &str, source: &str) -> bool {
        !package.is_empty() && (source.is_empty() || source == package || source.starts_with(&format!("{package}.")))
    }

    /// `private-code-leakage`, attribute form: a private attribute reached on a
    /// name bound by an import (`houses.school._try_geopoint`). One finding per
    /// distinct target in the file, at its first reach. The caller passes the
    /// reaches it drained, so the data flow is visible and a second call cannot
    /// find an empty cache.
    fn private_code_leakage_findings(&mut self, reaches: Vec<(String, String, usize, String)>) {
        let package = Self::package_of(self.file);
        let facade = self.file.ends_with("__init__.py");
        let mut seen = std::collections::HashSet::new();
        for (root, target, line, function) in reaches {
            let Some(source) = self.import_sources.get(&root) else {
                continue;
            };
            // A package's `__init__.py` reassembles its own package's names:
            // that is the facade's job, not leakage. A peer module may not.
            if facade && Self::same_package(&package, source) {
                continue;
            }
            if !seen.insert(target.clone()) {
                continue;
            }
            self.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: self.file.to_string(),
                line,
                col: 0,
                function,
                kind: "private-code-leakage".into(),
                severity: "fail".into(),
                message: format!(
                    concat!(
                        "reaches private '{target}' from another module — the name is an implementation detail, ",
                        "and reaching it couples this code to a choice the owner may change. ",
                        "Renaming it public is not the fix: that turns a detail into a promise. ",
                        "Call the public API that expresses the need, or move the call into the owning module. ",
                        "Make it public only when it is part of the module's contract."
                    ),
                    target = target
                ),
            });
        }
    }
}
