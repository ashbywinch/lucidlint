//! The public helpers around the complexity visitor — radon's
//! `cc_rank`/`average_complexity`/`sorted_results`/`add_inner_blocks` surface.
//! The visitor's counting rules live in `test_complexity_visitor.rs` (the
//! radon parity table); this file pins the helper layer the scanner calls.

use radonc::complexity::{
    add_inner_blocks, alpha, average_complexity, cc_visit, cc_visit_ast, lines, score,
    sorted_results, Order,
};
use radonc::visitors::{get_complexity, Block, Class, Function};

fn blocks(code: &str) -> Vec<Block> {
    cc_visit(code)
}

const TWO_FUNCTIONS: &str = "def beta():\n    if a: pass\n\ndef alpha():\n    pass\n";

#[test]
fn average_complexity_of_no_blocks_is_zero() {
    assert_eq!(average_complexity(&[]), 0.0);
}

#[test]
fn average_complexity_averages_the_blocks() {
    let found = blocks(TWO_FUNCTIONS);
    let expected = found.iter().map(get_complexity).sum::<i32>() as f64 / found.len() as f64;
    assert_eq!(average_complexity(&found), expected);
}

#[test]
fn sort_keys_read_complexity_line_and_name() {
    let found = blocks(TWO_FUNCTIONS);
    assert_eq!(score(&found[0]), -get_complexity(&found[0]));
    assert_eq!(lines(&found[0]), found[0].lineno());
    assert_eq!(alpha(&found[0]), found[0].name());
}

#[test]
fn sorted_results_honours_each_order() {
    let by_score = sorted_results(blocks(TWO_FUNCTIONS), Order::Score);
    assert!(get_complexity(&by_score[0]) >= get_complexity(&by_score[1]));

    let by_lines = sorted_results(blocks(TWO_FUNCTIONS), Order::Lines);
    assert!(lines(&by_lines[0]) <= lines(&by_lines[1]));

    let by_alpha = sorted_results(blocks(TWO_FUNCTIONS), Order::Alpha);
    assert_eq!(alpha(&by_alpha[0]), "alpha");
}

fn function(name: &str, complexity: i32) -> Function {
    Function {
        name: name.to_string(),
        lineno: 1,
        col_offset: 0,
        endline: 2,
        is_method: false,
        classname: None,
        closures: Vec::new(),
        inner_classes: Vec::new(),
        complexity,
    }
}

fn class(name: &str, methods: Vec<Function>, complexity: i32) -> Class {
    Class {
        name: name.to_string(),
        lineno: 1,
        col_offset: 0,
        endline: 2,
        inner_classes: Vec::new(),
        methods,
        complexity,
    }
}

#[test]
fn add_inner_blocks_lifts_closures_and_inner_classes() {
    // radon's add_inner_blocks: a closure and a function-local class become
    // top-level blocks named after their owner. The visitor does not collect
    // these yet (the port has no inner-block pass), so the blocks are built
    // directly — the helper's contract is what is under test.
    let outer = Function {
        closures: vec![function("inner", 1)],
        inner_classes: vec![class("Local", Vec::new(), 1)],
        ..function("outer", 2)
    };
    let lifted = add_inner_blocks(vec![Block::Function(outer), Block::Class(class("Solo", Vec::new(), 1))]);
    let names: Vec<&str> = lifted.iter().map(|b| b.name()).collect();
    assert!(names.contains(&"outer"), "{names:?}");
    assert!(names.contains(&"outer.inner"), "{names:?}");
    assert!(names.contains(&"outer.Local"), "{names:?}");
    assert!(names.contains(&"Solo"), "{names:?}");
}

#[test]
fn cc_visit_ast_matches_cc_visit() {
    let code = "def f():\n    if a: pass\n";
    let parsed = ruff_python_parser::parse_module(code).expect("the source parses");
    let from_ast = cc_visit_ast(parsed.syntax());
    let from_code = cc_visit(code);
    assert_eq!(from_ast.len(), from_code.len());
    assert_eq!(get_complexity(&from_ast[0]), get_complexity(&from_code[0]));
}
