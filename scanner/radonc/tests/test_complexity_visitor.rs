//! Mirrored from radon's tests/test_complexity_visitor.py — same case data,
//! same structure, ported to Rust. Reviewers diff this against upstream
//! radon to keep the port in sync.

use radonc::complexity::{cc_rank, cc_visit};
use radonc::visitors::ComplexityVisitor;

fn dedent(code: &str) -> String {
    // strip leading newline + common indentation (like textwrap.dedent)
    let lines: Vec<&str> = code.trim_start_matches('\n').lines().collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.len() >= indent { &l[indent..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// SIMPLE_BLOCKS from radon's test file: (code, expected_complexity, kwargs)
fn simple_blocks() -> Vec<(&'static str, i32)> {
    vec![
        ("if a: pass", 2),
        ("if a: pass\nelse: pass", 2),
        ("if a: pass\nelif b: pass", 3),
        ("if a: pass\nelif b: pass\nelse: pass", 3),
        ("if a and b: pass", 3),
        ("if a and b: pass\nelse: pass", 3),
        ("if a and b: pass\nelif c and d: pass\nelse: pass", 5),
        ("if a and b or c and d: pass\nelse: pass", 5),
        ("for i in range(10): print(i)", 2),
        ("for i in range(10): print(i)\nelse: pass", 3),
        ("while a < 4: pass", 2),
        ("while a < 4: pass\nelse: pass", 3),
        ("while a < 4 and b < 42: pass", 3),
        ("[i for i in range(4)]", 2),
        ("[i for i in range(4) if i & 1]", 3),
        ("(i for i in range(4))", 2),
        ("(i for i in range(4) if i & 1)", 3),
        ("try: raise TypeError\nexcept TypeError: pass", 2),
        ("try: raise TypeError\nexcept TypeError: pass\nelse: pass", 3),
        ("k = lambda a, b: k(b, a)", 1),
        ("k = lambda a, b, c: c if a else b", 2),
        ("v = a if b else c", 2),
        ("sum(i for i in range(12) for z in range(i) if i)", 4),
        ("assert i < 0", 2),
        ("assert i < 0, \"Fail\"", 2),
    ]
}

fn module_cc(code: &str) -> i32 {
    let visitor = ComplexityVisitor::from_code(&dedent(code));
    visitor.complexity
}

fn function_cc_of(code: &str) -> i32 {
    let visitor = ComplexityVisitor::from_code(&dedent(code));
    visitor.functions.iter().map(|f| f.complexity).sum::<i32>()
}

/// test_visitor_simple — the SIMPLE_BLOCKS table from radon's tests.
/// Note: the original runs these as module-level code (complexity = 1 + decisions);
/// the values match radon's table directly.
#[test]
fn test_visitor_simple() {
    for (code, expected) in simple_blocks() {
        // wrap as a function so decisions land inside one function like radon's
        let wrapped = format!("def f():\n    {}", code.replace('\n', "\n    ").trim_end());
        let visitor = ComplexityVisitor::from_code(wrapped.as_str());
        assert_eq!(
            visitor.functions[0].complexity, expected,
            "code:\n{}",
            wrapped
        );
    }
}

/// test_visitor_single_functions — (code, (module_complexity_diff, fn_complexity))
#[test]
fn test_visitor_single_functions() {
    let cases = [
        (
            "def f(a, b, c):\n    if a and b == 4:\n        return c ** c\n    elif a and not c:\n        return sum(i for i in range(41) if i & 1)\n    return a + b",
            7,
        ),
        (
            "if a and not b: pass\nelif b or c: pass\nelse: pass\n\ndef g(a, b):\n    while a < b:\n        b, a = a ** 2, b ** 2\n    return b",
            2,
        ),
        (
            "def f(a, b):\n    while a ** b:\n        a, b = b, a * (b - 1)\n        if a and b:\n            b = 0\n        else:\n            b = 1\n    return sum(i for i in range(b))",
            5,
        ),
    ];
    for (code, expected_fn_cc) in cases {
        let visitor = ComplexityVisitor::from_code(dedent(code).as_str());
        assert_eq!(visitor.functions.len(), 1);
        assert_eq!(visitor.functions[0].complexity, expected_fn_cc, "code:\n{code}");
    }
}

/// test_visitor_classes — CLASSES_CASES: (total_class_complexity, methods...)
#[test]
fn test_visitor_classes_cc() {
    let cases = [
        (
            "class A(object):\n\n    def m(self, a, b):\n        if not a or b:\n            return b - 1\n        try:\n            return a / b\n        except ZeroDivisionError:\n            return a\n\n    def n(self, k):\n        while self.m(k) < k:\n            k -= self.m(k ** 2 - min(self.m(j) for j in range(k ** 4)))\n        return k",
            vec![8, 4, 3],
        ),
        (
            "class B(object):\n\n    ATTR = 1 if A().n(1) == 1 else 2\n    import sys\n    if sys.version_info >= (3, 3):\n        import os\n        AT = os.open('/random/loc')\n\n    def __iter__(self):\n        return __import__('itertools').tee(B.__dict__)\n\n    def test(self, func):\n        a = func(self.ATTR, self.AT)\n        if a < self.ATTR:\n            yield self\n        elif a > self.ATTR ** 2:\n            yield self.__iter__()\n        yield iter(a)",
            vec![7, 1, 3],
        ),
    ];
    for (code, expected) in cases {
        let visitor = ComplexityVisitor::from_code(dedent(code).as_str());
        assert_eq!(visitor.classes.len(), 1, "code:\n{code}");
        assert_eq!(visitor.functions.len(), 0);
        // radon's test_visitor_classes: cls.real_complexity == expected[0],
        // methods == expected[1:] (class total + each method's complexity)
        assert_eq!(visitor.classes[0].real_complexity(), expected[0], "code:\n{code}");
        let methods: Vec<i32> = visitor.classes[0].methods.iter().map(|m| m.complexity).collect();
        assert_eq!(methods.as_slice(), &expected[1..], "code:\n{code}");
    }
}

/// test_visitor_module — the GENERAL_CASES: (module, functions, classes, total)
#[test]
fn test_visitor_module() {
    let cases = [
        (
            "if a and b:\n    print\nelse:\n    print\na = sum(i for i in range(1000) if i % 3 == 0 and i % 5 == 0)\n\ndef f(n):\n    def inner(n):\n        return n ** 2\n\n    if n == 0:\n        return 1\n    elif n == 1:\n        return n\n    elif n < 5:\n        return (n - 1) ** 2\n    return n * pow(inner(n), f(n - 1), n - 3)\n",
            (6, 3, 0, 9),
        ),
    ];
    for (code, (module, fns, classes, total)) in cases {
        let visitor = ComplexityVisitor::from_code(dedent(code).as_str());
        assert_eq!(visitor.complexity, module);
        assert_eq!(visitor.functions_complexity(), fns);
        assert_eq!(visitor.classes_complexity(), classes);
        assert_eq!(visitor.total_complexity(), total);
    }
}

/// cc_rank — the A–F table
#[test]
fn test_cc_rank() {
    assert_eq!(cc_rank(1), 'A');
    assert_eq!(cc_rank(5), 'A');
    assert_eq!(cc_rank(6), 'B');
    assert_eq!(cc_rank(10), 'B');
    assert_eq!(cc_rank(11), 'C');
    assert_eq!(cc_rank(20), 'C');
    assert_eq!(cc_rank(21), 'D');
    assert_eq!(cc_rank(30), 'D');
    assert_eq!(cc_rank(31), 'E');
    assert_eq!(cc_rank(40), 'E');
    assert_eq!(cc_rank(41), 'F');
    assert_eq!(cc_rank(100), 'F');
}

/// cc_visit — the top-level API: returns blocks, like radon's cc_visit
#[test]
fn test_cc_visit_blocks() {
    let blocks = cc_visit("def f():\n    if a:\n        return 1\n    return 0\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].name(), "f");
    assert_eq!(blocks[0].complexity(), 2);
}
