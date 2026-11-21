//! Forwarding-chain raw material: a function or method whose body forwards,
//! and the repo-wide method index the chain rule reads.

/// A module-level function whose body is exactly `return <callee>(<all
/// params>)` — the middle edge of a forwarding chain (#30).
#[derive(Clone)]
pub struct ForwarderFn {
    pub rel: String,
    pub name: String,
    pub params: Vec<String>,
    /// "Fn:<name>" for a module-fn callee, "Method:<recv-param>:<method>" for
    /// a method callee on another class.
    pub callee: String,
}

/// A class method whose body is exactly `return F(<all params>)` with F a
/// module-level function — the head of a forwarding chain (#30).
#[derive(Clone)]
pub struct ForwarderMethod {
    pub rel: String,
    pub class: String,
    pub name: String,
    pub line: usize,
    pub params: Vec<String>, // minus self/cls
    pub fn_callee: String,
}

/// Every method of every top-level class (params minus receiver) — the
/// terminal pool the forwarding-chain detector resolves `recv.method(...)`
/// callees against (#30, R9).
#[derive(Clone)]
pub struct RepoMethod {
    pub rel: String,
    pub class: String,
    pub name: String,
    pub line: usize,
    pub params: Vec<String>,
}
