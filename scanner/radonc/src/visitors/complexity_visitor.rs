//! The result of visiting a module: per-statement complexity totals, like
//! radon's `ComplexityVisitor` instance.

use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
use ruff_python_ast::{ModModule, Stmt};

use super::{code2ast, Block, Class, CountVisitor, Function};

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
    /// The module source — present when built via `from_code`, so block
    /// linenos are SOURCE LINES; `from_ast` has no source and keeps the
    /// ruff byte offsets (documented deviation).
    source: Option<String>,
}

impl ComplexityVisitor {
    /// Instantiate the visitor from Python source (radon's `from_code`).
    pub fn from_code(code: &str) -> Self {
        // the source must be attached BEFORE the walk — the linenos are
        // computed during it (from_ast has no source and keeps offsets)
        let parsed = code2ast(code);
        let mut visitor = ComplexityVisitor::default();
        visitor.source = Some(code.to_string());
        visitor.visit(&parsed.syntax());
        visitor
    }

    /// Instantiate from a parsed module (radon's `from_ast`). Block linenos
    /// are byte offsets when no source is attached.
    pub fn from_ast(mod_: &ModModule) -> Self {
        let mut visitor = ComplexityVisitor::default();
        visitor.visit(mod_);
        visitor
    }

    /// Byte offset -> 1-based source line (or the offset when no source).
    fn line_of(&self, offset: usize) -> i32 {
        match &self.source {
            Some(src) => {
                (src.as_bytes()[..offset.min(src.len())]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count()
                    + 1) as i32
            }
            None => offset as i32,
        }
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
        out.extend(self.functions.iter().map(|f| Block::Function(f.clone())));
        for c in &self.classes {
            out.push(Block::Class(c.clone()));
            out.extend(c.methods.iter().map(|m| Block::Function(m.clone())));
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
                    let lineno = self.line_of(f.range.start().to_usize());
                    let endline = self.line_of(f.range.end().to_usize());
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
                        complexity: CountVisitor::function_cc(f, self.no_assert),
                    });
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.to_string();
                    let lineno = self.line_of(c.range.start().to_usize());
                    let endline = self.line_of(c.range.end().to_usize());
                    let col = 0;
                    // methods
                    let mut methods = Vec::new();
                    methods.extend(c.body.iter().filter_map(|body_stmt| match body_stmt {
                        Stmt::FunctionDef(m) => Some(Function {
                            name: m.name.to_string(),
                            lineno: self.line_of(m.range.start().to_usize()),
                            col_offset: 0,
                            endline: self.line_of(m.range.end().to_usize()),
                            is_method: true,
                            classname: Some(c.name.to_string()),
                            closures: Vec::new(),
                            inner_classes: Vec::new(),
                            complexity: CountVisitor::function_cc(m, self.no_assert),
                        }),
                        _ => None,
                    }));
                    self.classes.push(Class {
                        name,
                        lineno,
                        col_offset: col,
                        endline,
                        inner_classes: Vec::new(),
                        methods,
                        complexity: CountVisitor::class_cc(c, self.no_assert),
                    });
                }
                _ => {}
            }
        }
        self.complexity = counter.d + 1;
    }
}
