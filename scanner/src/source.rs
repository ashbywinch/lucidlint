//! The line index: offsets to 1-based line and column, and a statement's line.
//!
//! One thread-local cache per source text, rebuilt in O(n) and searched by
//! binary search; every finding carries a line, so this is on the hot path.

use ruff_python_ast::Stmt;
use ruff_text_size::TextSize;

/// The source text and its line index: positions to line numbers, and statement starts.
pub struct Source;

// 2000-line file of function defs paid ~10ms of line scans per 1000 defs.
// The index is rebuilt once per distinct source (O(n)), then binary search.
thread_local! {
    static LINE_STARTS: std::cell::RefCell<(usize, usize, Vec<usize>)> =
        const { std::cell::RefCell::new((0, 0, Vec::new())) };
}

impl Source {
    /// 1-based line of an offset.
    /// The LINE_STARTS cache is keyed by (ptr, len) — a fresh String can reuse
    /// the previous file's buffer address at the same length, so the key alone
    /// is unsafe; force a rebuild for every scan (review bot).
    pub(crate) fn reset_line_starts() {
        LINE_STARTS.with(|c| {
            let mut c = c.borrow_mut();
            c.0 = 0;
            c.1 = 0;
            c.2.clear();
        });
    }

    pub(crate) fn line_of(source: &str, offset: TextSize) -> usize {
        LINE_STARTS.with(|c| {
            let mut c = c.borrow_mut();
            if c.0 != source.as_ptr() as usize || c.1 != source.len() {
                c.0 = source.as_ptr() as usize;
                c.1 = source.len();
                c.2.clear();
                c.2.push(0);
                for (i, b) in source.bytes().enumerate() {
                    if b == b'\n' {
                        c.2.push(i + 1);
                    }
                }
            }
            c.2.partition_point(|&s| s <= offset.to_usize())
        })
    }

    /// 1-based column of an offset — the schema-3 disambiguation coordinate.
    pub(crate) fn col_of(source: &str, offset: TextSize) -> usize {
        LINE_STARTS.with(|c| {
            let mut c = c.borrow_mut();
            if c.0 != source.as_ptr() as usize || c.1 != source.len() {
                c.0 = source.as_ptr() as usize;
                c.1 = source.len();
                c.2.clear();
                c.2.push(0);
                for (i, b) in source.bytes().enumerate() {
                    if b == b'\n' {
                        c.2.push(i + 1);
                    }
                }
            }
            let off = offset.to_usize();
            off - c.2[c.2.partition_point(|&s| s <= off) - 1] + 1
        })
    }

    pub(crate) fn stmt_line(source: &str, stmt: &Stmt) -> usize {
        let off = match stmt {
            Stmt::FunctionDef(f) => f.range.start(),
            Stmt::ClassDef(c) => c.range.start(),
            Stmt::Return(r) => r.range.start(),
            Stmt::Delete(d) => d.range.start(),
            Stmt::TypeAlias(t) => t.range.start(),
            Stmt::Assign(a) => a.range.start(),
            Stmt::AugAssign(a) => a.range.start(),
            Stmt::AnnAssign(a) => a.range.start(),
            Stmt::For(f) => f.range.start(),
            Stmt::While(w) => w.range.start(),
            Stmt::If(i) => i.range.start(),
            Stmt::With(w) => w.range.start(),
            Stmt::Match(m) => m.range.start(),
            Stmt::Raise(r) => r.range.start(),
            Stmt::Try(t) => t.range.start(),
            Stmt::Assert(a) => a.range.start(),
            Stmt::Import(i) => i.range.start(),
            Stmt::ImportFrom(i) => i.range.start(),
            Stmt::Global(g) => g.range.start(),
            Stmt::Nonlocal(n) => n.range.start(),
            Stmt::Expr(e) => e.range.start(),
            Stmt::Pass(p) => p.range.start(),
            Stmt::Break(b) => b.range.start(),
            Stmt::Continue(c) => c.range.start(),
            Stmt::IpyEscapeCommand(s) => s.range.start(),
        };
        Self::line_of(source, off)
    }
}
