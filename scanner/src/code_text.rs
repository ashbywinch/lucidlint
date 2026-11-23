//! The literal-text vocabulary shared by the Python and Rust
//! `code-in-literal` detectors: whether a string value is a code block, and
//! which helper names take a code snippet as their argument.

/// The predicates over a string literal's text.
pub(crate) struct CodeText;

impl CodeText {
    /// Line-start keywords that mark a line as code rather than prose.
    const CODE_KEYWORDS: &[&str] = &[
        "fn", "def", "class", "let", "impl", "use", "import", "for", "if", "match",
    ];

    /// Bare scan/compile/exec helpers whose first string argument is a program
    /// being fed to the tool — the snippet is the input, never prose.
    const BARE_HELPERS: &[&str] = &["scan", "scan_src", "scan_source", "compile", "exec", "eval"];

    /// A module- or method-qualified helper is the same call
    /// (`helpers.scan_src`, `self.scan_src`). Only the scan family qualifies:
    /// `re.compile` is a regex, so compile/exec/eval stay bare-name only.
    const QUALIFIED_HELPERS: &[&str] = &["scan", "scan_src", "scan_source"];

    /// The literal holds a code block: at least two non-empty lines, with one
    /// line that reads as a statement of its keyword, not prose that happens
    /// to start with the same word. A one-line message, a regex, and a
    /// docstring's example are not blocks.
    pub(crate) fn is_code_block(value: &str) -> bool {
        let lines: Vec<&str> = value.lines().filter(|line| !line.trim().is_empty()).collect();
        if lines.len() < 2 {
            return false;
        }
        lines.iter().any(|line| Self::is_code_line(line.trim_start()))
    }

    /// A bare name (or a path's last segment) names a helper that takes a code
    /// snippet.
    pub(crate) fn is_code_helper(name: &str) -> bool {
        Self::BARE_HELPERS.contains(&name)
    }

    /// A qualified receiver's final attribute names a scan helper that takes a
    /// code snippet.
    pub(crate) fn is_qualified_code_helper(name: &str) -> bool {
        Self::QUALIFIED_HELPERS.contains(&name)
    }

    /// A one-line helper argument that is code: punctuation or an
    /// escape-produced newline. A bare filename (`"app.py"`, `"a.py"`) is not
    /// a snippet.
    pub(crate) fn is_code_snippet(value: &str) -> bool {
        Self::is_code_block(value)
            || value.contains('\n')
            || value.chars().any(|c| matches!(c, '{' | '}' | '(' | ')' | '=' | ':'))
    }

    fn is_code_line(line: &str) -> bool {
        Self::CODE_KEYWORDS.iter().any(|keyword| {
            line.strip_prefix(keyword)
                .and_then(|rest| rest.strip_prefix(char::is_whitespace))
                .is_some_and(|rest| Self::code_shape(keyword, rest.trim_start()))
        })
    }

    /// The statement shape after the keyword — the part prose fails: a name
    /// and a call/block marker for a declaration, an assignment or terminator
    /// for a binding, a block or iterable for a control form, a module path
    /// for a use.
    fn code_shape(keyword: &str, rest: &str) -> bool {
        match keyword {
            "fn" | "def" | "class" => {
                Self::has_identifier(rest) && (rest.contains('(') || rest.ends_with(':') || rest.ends_with('{'))
            }
            "let" => rest.contains('=') || rest.ends_with(';'),
            "impl" => rest.ends_with('{') || rest.ends_with(':'),
            "use" | "import" => Self::is_module_path(rest),
            "for" | "if" | "match" => rest.ends_with('{') || rest.ends_with(':') || rest.contains(" in "),
            _ => false,
        }
    }

    fn has_identifier(rest: &str) -> bool {
        rest.starts_with(|c: char| c.is_alphabetic() || c == '_')
    }

    fn is_module_path(rest: &str) -> bool {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '*' | ',' | '{' | '}' | ' '))
    }
}

#[cfg(test)]
mod tests {
    use super::CodeText;

    #[test]
    fn qualified_helpers_are_the_scan_family_only() {
        assert!(CodeText::is_qualified_code_helper("scan_src"));
        assert!(CodeText::is_qualified_code_helper("scan_source"));
        assert!(!CodeText::is_qualified_code_helper("compile"), "re.compile is a regex");
        assert!(!CodeText::is_qualified_code_helper("exec"));
    }

    #[test]
    fn bare_helpers_keep_compile_exec_eval() {
        assert!(CodeText::is_code_helper("compile"));
        assert!(CodeText::is_code_helper("exec"));
        assert!(CodeText::is_code_helper("eval"));
    }
}
