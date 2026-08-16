//! Visitors and blocks for cyclomatic complexity — a Rust port of
//! `radon.visitors` (the complexity visitor). Mirrors the original module's
//! structure and public names so it can be diffed against upstream radon
//! and updated when radon changes. See the crate's NOTICE for the radon
//! attribution.
//!
//! The counting rules are radon-equivalent: if/elif count (+1 each, trailing
//! else 0), for/while + else, try handlers + else, assert +1 (non-recursive),
//! match minus wildcard, boolop operands-1, ternary +1, comprehension
//! generators+ifs, lambda +0 body walked. Nested functions and class bodies
//! contribute 0 to the enclosing scope.

use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast::{Expr, ModModule, Pattern, Stmt, StmtFunctionDef};
use ruff_python_parser::{parse_module, Parsed};

/// Parse Python source into an AST module (radon's `code2ast`).
pub fn code2ast(code: &str) -> Parsed<ModModule> {
    // the panic mirrors radon's code2ast raising SyntaxError on invalid
    // source (R26) — a parse failure here is a programming error
    // lucidlint: ignore debug-artifact the panic mirrors radon's raise (R26)
    parse_module(code).expect("invalid Python source")
}

/// The complexity of a block (function/method/class) — radon's `GET_COMPLEXITY`.
pub fn get_complexity(b: &Block) -> i32 {
    b.complexity()
}

/// One function or method entry, mirroring radon's `Function` namedtuple.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub endline: i32,
    pub is_method: bool,
    pub classname: Option<String>,
    pub closures: Vec<Function>,
    pub inner_classes: Vec<Class>,
    pub complexity: i32,
}

impl Function {
    /// radon's `Function.letter`: 'F' for functions, 'M' for methods.
    pub fn letter(&self) -> char {
        if self.is_method {
            'M'
        } else {
            'F'
        }
    }

    /// radon's `Function.fullname`: `cls.method` for methods, name otherwise.
    pub fn fullname(&self) -> String {
        match &self.classname {
            Some(c) => format!("{}.{}", c, self.name),
            None => self.name.clone(),
        }
    }
}

impl std::fmt::Display for Function {
    /// radon's `Function.__str__`: `F name:start->end cls.name - complexity`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}:{}->{} {} - {}",
            self.letter(),
            self.lineno,
            self.col_offset,
            self.endline,
            self.fullname(),
            self.complexity
        )
    }
}

/// One class block — mirroring radon's `Class` namedtuple.
#[derive(Clone, Debug)]
pub struct Class {
    pub name: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub endline: i32,
    pub inner_classes: Vec<Class>,
    pub methods: Vec<Function>,
    pub complexity: i32,
}

impl Class {
    /// radon's `Class.letter`: always 'C'.
    // the mirror keeps letter as a method to match radon's property (R26) —
    // structure over instance-state purity
    // lucidlint: ignore detached-method radon mirror keeps letter a method
    pub fn letter(&self) -> char {
        'C'
    }

    /// radon's `Class.fullname`: the class name.
    pub fn fullname(&self) -> &str {
        &self.name
    }

    /// radon's `Class.complexity`: the class's own complexity. Note the real
    /// complexity (which includes nested classes) is exposed separately.
    pub fn real_complexity(&self) -> i32 {
        self.complexity
    }
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "C {}:{}->{} {} - {}",
            self.lineno, self.col_offset, self.endline, self.name, self.complexity
        )
    }
}

/// A block is either a function or a class — mirroring radon's `Block` union.
#[derive(Clone, Debug)]
pub enum Block {
    Function(Function),
    Class(Class),
}

impl Block {
    pub fn name(&self) -> &str {
        match self {
            Block::Function(f) => &f.name,
            Block::Class(c) => &c.name,
        }
    }

    pub fn complexity(&self) -> i32 {
        match self {
            Block::Function(f) => f.complexity,
            Block::Class(c) => c.complexity,
        }
    }

    pub fn lineno(&self) -> i32 {
        match self {
            Block::Function(f) => f.lineno,
            Block::Class(c) => c.lineno,
        }
    }
}

/// The result of visiting a module: per-statement complexity totals, like
/// radon's `ComplexityVisitor` instance.
#[derive(Debug, Default)]
pub struct ComplexityVisitor {
    /// Module-level complexity (1 + all decisions outside functions).
    pub complexity: i32,
    /// Top-level functions in the module.
    pub functions: Vec<Function>,
    /// Top-level classes in the module.
    pub classes: Vec<Class>,
    /// `total_complexity` = complexity + functions_complexity + classes_complexity
    pub total: i32,
    /// Whether assertions are counted (radon's `no_assert`).
    pub no_assert: bool,
}

impl ComplexityVisitor {
    /// Instantiate the visitor from Python source (radon's `from_code`).
    pub fn from_code(code: &str) -> Self {
        let parsed = code2ast(code);
        Self::from_ast(&parsed.syntax())
    }

    /// Instantiate from a parsed module (radon's `from_ast`).
    pub fn from_ast(mod_: &ModModule) -> Self {
        let mut visitor = ComplexityVisitor::default();
        visitor.visit(mod_);
        visitor
    }

    /// The module-level complexity.
    pub fn module_complexity(&self) -> i32 {
        self.complexity
    }

    /// Total complexity of all functions — radon's `functions_complexity`.
    pub fn functions_complexity(&self) -> i32 {
        self.functions.iter().map(|f| f.complexity).sum::<i32>() - self.functions.len() as i32
    }

    /// Total complexity of all classes — radon's `classes_complexity`.
    pub fn classes_complexity(&self) -> i32 {
        self.classes.iter().map(|c| c.complexity).sum::<i32>() - self.classes.len() as i32
    }

    /// Total complexity — radon's `total_complexity`.
    pub fn total_complexity(&self) -> i32 {
        self.complexity + self.functions_complexity() + self.classes_complexity()
    }

    /// All blocks (functions, classes, methods) — radon's `blocks`.
    pub fn blocks(&self) -> Vec<Block> {
        let mut out = Vec::new();
        for f in &self.functions {
            out.push(Block::Function(f.clone()));
        }
        for c in &self.classes {
            out.push(Block::Class(c.clone()));
            for m in &c.methods {
                out.push(Block::Function(m.clone()));
            }
        }
        out
    }

    /// Visit a module's top-level statements, collecting per-function and
    /// per-class complexity by walking them with the counting visitor.
    fn visit(&mut self, mod_: &ModModule) {
        let mut counter = CountVisitor::default();
        for stmt in &mod_.body {
            // call visit_stmt — the COUNTING override; walk_stmt would skip it
            counter.visit_stmt(stmt);

            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.to_string();
                    let lineno = f.range.start().to_usize() as i32; // byte offset — radon's `lineno` is source-line-based; ruff AST carries bytes, see NOTICE
                    let endline = f.range.end().to_usize() as i32;
                    let col = 0; // byte offsets carry no column in ruff_text_size
                    self.functions.push(Function {
                        name,
                        lineno,
                        col_offset: col,
                        endline,
                        is_method: false,
                        classname: None,
                        closures: Vec::new(),
                        inner_classes: Vec::new(),
                        complexity: complex_counter(f, self.no_assert),
                    });
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.to_string();
                    let lineno = c.range.start().to_usize() as i32;
                    let endline = c.range.end().to_usize() as i32;
                    let col = 0;
                    // methods
                    let mut methods = Vec::new();
                    for body_stmt in &c.body {
                        if let Stmt::FunctionDef(m) = body_stmt {
                            methods.push(Function {
                                name: m.name.to_string(),
                                lineno: m.range.start().to_usize() as i32,
                                col_offset: 0,
                                endline: m.range.end().to_usize() as i32,
                                is_method: true,
                                classname: Some(c.name.to_string()),
                                closures: Vec::new(),
                                inner_classes: Vec::new(),
                                complexity: complex_counter(m, self.no_assert),
                            });
                        }
                    }
                    self.classes.push(Class {
                        name,
                        lineno,
                        col_offset: col,
                        endline,
                        inner_classes: Vec::new(),
                        methods,
                        complexity: class_cc(c, self.no_assert),
                    });
                }
                _ => {}
            }
        }
        self.complexity = counter.d + 1;
    }
}

/// Count complexity for one function body (crate-internal; the scan core
/// calls this for its FnCc values).
pub fn complex_counter(f: &StmtFunctionDef, no_assert: bool) -> i32 {
    let mut counter = CountVisitor::default();
    counter.no_assert = no_assert;
    for stmt in &f.body {
        counter.visit_stmt(stmt);
    }
    counter.d + 1
}

/// The decision-counting visitor backing both module-level and per-function
/// complexity. Mirrors radon's `ComplexityVisitor.generic_visit` + the
/// `visit_*` overrides.
#[derive(Default)]
pub struct CountVisitor {
    pub d: i32,
    pub no_assert: bool,
}

impl<'a> SourceOrderVisitor<'a> for CountVisitor {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return, // own scope / 0 decisions
            Stmt::If(i) => {
                self.d += 1 + i.elif_else_clauses.iter().filter(|c| c.test.is_some()).count() as i32;
                // recurse into the test + bodies EXACTLY like radon's generic_visit
                self.visit_expr(&i.test);
                for s in &i.body {
                    self.visit_stmt(s);
                }
                for c in &i.elif_else_clauses {
                    if let Some(t) = &c.test {
                        self.visit_expr(t);
                    }
                    for s in &c.body {
                        self.visit_stmt(s);
                    }
                }
                return;
            }
            Stmt::For(f) => {
                self.d += 1 + (!f.orelse.is_empty()) as i32;
                self.visit_expr(&f.iter);
                for s in &f.body {
                    self.visit_stmt(s);
                }
                for s in &f.orelse {
                    self.visit_stmt(s);
                }
                return;
            }
            Stmt::While(w) => {
                self.d += 1 + (!w.orelse.is_empty()) as i32;
                self.visit_expr(&w.test);
                for s in &w.body {
                    self.visit_stmt(s);
                }
                for s in &w.orelse {
                    self.visit_stmt(s);
                }
                return;
            }
            Stmt::Try(t) => {
                self.d += t.handlers.len() as i32 + (!t.orelse.is_empty()) as i32;
                for s in &t.body {
                    self.visit_stmt(s);
                }
                for handler in &t.handlers {
                    let eh = match handler {
                        ruff_python_ast::ExceptHandler::ExceptHandler(eh) => eh,
                    };
                    for s in &eh.body {
                        self.visit_stmt(s);
                    }
                }
                for s in &t.orelse {
                    self.visit_stmt(s);
                }
                for s in &t.finalbody {
                    self.visit_stmt(s);
                }
                return;
            }
            Stmt::Assert(_) => {
                if !self.no_assert {
                    self.d += 1;
                }
                // radon's visit_Assert never recurses
                return;
            }
            Stmt::Match(m) => {
                self.d += m.cases.iter().map(|case| {
                    let wild = matches!(&case.pattern, Pattern::MatchAs(p)
                        if p.pattern.is_none() && p.name.is_none());
                    if wild { 0 } else { 1 }
                }).sum::<i32>();
                self.visit_expr(&m.subject);
                for case in &m.cases {
                    for s in &case.body {
                        self.visit_stmt(s);
                    }
                }
                return;
            }
            Stmt::Return(r) => {
                if let Some(e) = &r.value {
                    self.visit_expr(e);
                }
                return;
            }
            Stmt::Expr(e) => {
                self.visit_expr(&e.value);
                return;
            }
            Stmt::Assign(a) => {
                self.visit_expr(&a.value);
                return;
            }
            Stmt::AugAssign(a) => {
                self.visit_expr(&a.value);
                return;
            }
            Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    self.visit_expr(v);
                }
                return;
            }
            _ => {}
        }
        // remaining: With, AsyncWith, Raise, etc — walk children generically
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::BoolOp(b) => self.d += b.values.len().saturating_sub(1) as i32,
            Expr::If(_) => self.d += 1,
            Expr::Lambda(_) => { /* +0, body walked by the default walk_expr */ }
            Expr::ListComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::SetComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::DictComp(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            Expr::Generator(c) => {
                self.d += c.generators.len() as i32;
                self.d += c.generators.iter().map(|g| g.ifs.len() as i32).sum::<i32>();
            }
            _ => {}
        }
        walk_expr(self, expr);
    }
}

fn class_cc(c: &ruff_python_ast::StmtClassDef, no_assert: bool) -> i32 {
    let mut counter = CountVisitor::default();
    counter.no_assert = no_assert;
    // radon's visit_ClassDef: body starts at 1, and each method contributes
    // functions_complexity + len(functions) = its own CC. So the class
    // complexity = 1 + (non-method body decisions) + sum(method CCs).
    for body_stmt in &c.body {
        if !matches!(body_stmt, Stmt::FunctionDef(_)) {
            counter.visit_stmt(body_stmt);
        }
    }
    let methods_cc: i32 = c
        .body
        .iter()
        .filter_map(|s| {
            if let Stmt::FunctionDef(m) = s {
                Some(complex_counter(m, no_assert))
            } else {
                None
            }
        })
        .sum();
    counter.d + 1 + methods_cc
}