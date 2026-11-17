//! `fakefs`: a test drives the real filesystem — its result depends on the
//! machine, and its writes outlive the test.

use ruff_python_ast::{AnyNodeRef, Expr, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::line_of;
use crate::ScanState;

const FS_PATH_METHODS: [&str; 17] = [
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "mkdir",
    "unlink",
    "rename",
    "replace",
    "touch",
    "rmdir",
    "iterdir",
    "glob",
    "rglob",
    "exists",
    "resolve",
    "symlink_to",
    "copy",
];

const FS_OS_OPS: [&str; 9] = [
    "remove", "rename", "mkdir", "makedirs", "rmdir", "unlink", "symlink", "link", "replace",
];

const FS_SHUTIL_OPS: [&str; 5] = ["copy", "copy2", "move", "rmtree", "copytree"];

const FS_TEMPFILE: [&str; 6] = [
    "TemporaryDirectory",
    "NamedTemporaryFile",
    "TemporaryFile",
    "mkdtemp",
    "mkstemp",
    "mktemp",
];

/// `_fakefs_findings` — real FS access in tests without pyfakefs.
pub fn fakefs_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let uses_fakefs_base = declares_fakefs_fixture(body, source);
    // all functions, with their enclosing test-class status
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                let line = line_of(source, f.name.range().start());
                let is_test = f.name.as_str().starts_with("test_")
                    || body.iter().any(|s| {
                        matches!(s, Stmt::ClassDef(c) if c.name.to_lowercase().starts_with("test")
                            && c.body.iter().any(|m| m.range().start() == n.range().start()))
                    });
                if !is_test {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                let uses_fakefs = f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .any(|p| p.parameter.name.as_str() == "fs");
                if uses_fakefs || uses_fakefs_base {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                let (real_fs, needs_real) = fs_usage(f);
                if real_fs && !needs_real {
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                            file: state.file.to_string(),
                                            line,
                                            function: f.name.to_string(),
                                            kind: "fakefs".into(),
                                            severity: "fail".into(),
                                            message: format!(
                                                "test '{}' at line {line} touches the real filesystem (tmp_path/open/Path) without pyfakefs — tests fake the filesystem (the `fs` fixture or fake_filesystem_unittest). Reach the real filesystem only when the code under test needs real FS semantics (subprocess interop, symlinks, C-level I/O like sqlite3), or when the test's subject is a checked-in file the repo ships (a workflow, a build script); comment why",
                                                f.name.as_str()
                                            ), });
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

/// The module sets up pyfakefs itself: the import, or a test base class named
/// for it.
fn declares_fakefs_fixture(body: &[Stmt], source: &str) -> bool {
    for s in body {
        match s {
            Stmt::ImportFrom(imp) => {
                if imp.module.as_ref().is_some_and(|m| m.to_string().contains("pyfakefs")) {
                    return true;
                }
            }
            Stmt::Import(imp) => {
                if imp.names.iter().any(|a| a.name.to_string().contains("pyfakefs")) {
                    return true;
                }
            }
            Stmt::ClassDef(cls) => {
                if let Some(arguments) = &cls.arguments {
                    for b in &arguments.args {
                        let base = source[b.range()].to_lowercase();
                        if base.contains("fakefs") || base.contains("fake_filesystem") {
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

/// What the test's body and decorators say about the filesystem: (touches the
/// real one, needs the real one). A skipif guard on a real file (`.exists()`) is
/// FS usage, so decorators count — ast.walk sees them.
fn fs_usage(f: &StmtFunctionDef) -> (bool, bool) {
    let mut real_fs = false;
    let mut needs_real = false;
    let mut fq: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    for d in &f.decorator_list {
        fq.push(Q::N(AnyNodeRef::from(&d.expression)));
    }
    let mut fi = 0usize;
    while fi < fq.len() {
        if let Q::N(node) = fq[fi] {
            match node {
                AnyNodeRef::ExprName(n) if n.id == "tmp_path" => real_fs = true,
                AnyNodeRef::ExprName(n) if matches!(n.id.as_str(), "sqlite3" | "subprocess") => needs_real = true,
                AnyNodeRef::ExprAttribute(a) if matches!(a.attr.as_str(), "symlink" | "symlink_to" | "link") => {
                    needs_real = true
                }
                AnyNodeRef::ExprCall(c) => match c.func.as_ref() {
                    // bare open(...)/tempfile(...) count; a Path(...).open(...)
                    // attribute call does NOT (Python's _real_fs_usage: only
                    // Name funcs match open/tempfile; attribute calls must be
                    // in the path/os/shutil/tempfile method lists)
                    Expr::Name(n) if matches!(n.id.as_str(), "open" | "tempfile") => {
                        real_fs = true;
                    }
                    Expr::Attribute(a) => {
                        let attr = a.attr.as_str();
                        if FS_PATH_METHODS.contains(&attr)
                            || FS_OS_OPS.contains(&attr)
                            || FS_SHUTIL_OPS.contains(&attr)
                            || FS_TEMPFILE.contains(&attr)
                        {
                            real_fs = true;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            skel_children(node, &mut fq);
        }
        fi += 1;
    }
    (real_fs, needs_real)
}
