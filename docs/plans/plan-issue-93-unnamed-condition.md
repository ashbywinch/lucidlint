# Plan — issue #93: `unnamed-condition` and `unnamed-classification`

Fixes github.com/ashbywinch/lucidlint issue #93. Every change ships with
fixtures and Rust unit tests; the two-repo run (houses, family-history-album)
is the acceptance instrument.

## The gap

Nothing named an inline compound condition. `complexity` is function-level
CC ≥ 15, `conditional-polymorphism` is a ≥4-arm dispatch chain, `special-case`
is repeated `None` checks on one name. A condition that joins three facts and
decides a branch had no name and no finding.

## What the measurement changed about the rule

The issue proposed an operator count (≥2 boolean operators). Measured on the
two house repos, that criterion fires 64 / 95 / 70 times in production Python,
and 61 / 87 / 57 of those are structural guards — the operator count and the
harm are uncorrelated. The criterion that matches a reader's judgment is the
**noun set**: the operands name one or more things, and a name is missing.

Every rejected variant is recorded so the next round does not re-litigate it:

| criterion | build-tools | houses | album |
|---|---|---|---|
| ≥2 boolean operators, production Python | 64 | 95 | 70 |
| … every operand a domain predicate (no `isinstance`/`None`) | 3 | 8 | 13 |
| … and no comparison at all | 3 | 4 | 2 |
| **shipped: ≥3 operands, operands name one or more nouns, not `self` alone** | **26 + 28** | **41 + 32** | **35 + 20** |

The "narrowed" sets read as guards by operand type but not by meaning
(`len(text) >= 7 and text[5:7].isdigit() and text[4] == "-"` is
`_looks_like_postcode`), which is why the shipped rule keeps them.

## The shape

- Python: an `if`/`while` test, a `return` value, an `assert` test, or a
  `match` guard whose top level is a `BoolOp` with ≥3 operands.
- Rust: an `if`/`while` condition whose top level is `&&`/`||` with ≥3
  operands.
- Exempt: the expression is the function's answer (it is returned and every
  other return is a literal early exit) — the function's name is the name;
  `self` alone (internal state on an object that already has a name);
  `__eq__`/`__ne__`/`__lt__`/`__le__`/`__gt__`/`__ge__`/`__hash__`/
  `__contains__`/`__bool__` and Rust's `eq`/`ne`/`cmp`/`partial_cmp`/`hash`
  (the operator is the decision); a parenthesised group that is about one noun
  and about nothing the rest of the condition is about is the unit reported
  (the reader already drew that boundary), provided it is itself three facts.
- Nouns are the receiver of an attribute/method chain, the subject of a
  comparison, or a free call's first argument. Names that name a type, a
  constant, or an import alias are the vocabulary the condition is written in,
  not things it decides about — they are dropped from the message.

## The message

The mechanism, then a resolved action: the object is a fact the AST holds.

- one noun: "… Name the decision as a predicate over `{noun}`, and call it here."
- several nouns: "… Name the decision as a predicate, and call it here."
- `self` among them: "… Name the decision as a method on `{class}`, and call it here."

The signature is never prescribed: "make it a method taking `{others}`" would
relocate the clump into a parameter list and can create a `data-clump` finding.

## The second arm

The case that motivated the issue (#88's inline `match` over `Expr` variants)
carries **zero boolean operators**, so the condition arm cannot reach it. The
classification arm does: a `match`/if-chain of ≥3 cases sorting one value into
constants, inline in a function that does more than the chain. A function whose
whole body is the match (the token mappers) is exempt — the function name is
the name. Python: 0 hits in all three repos. Rust: 0 live hits, one at the
pre-fix commit of `global_state_message`.

## Two rule defects the round exposed

- The Rust comment scanner lost one line per `\`-continued string literal, so
  every suppression after such a string drifted out of its window (a
  `large-function` ignore stopped binding). Fixed in `rs_comment_lines`.
- A `match` used as one argument of a call is an expression, not a named kind;
  the classification arm now requires the match to be its statement's whole
  expression.

## Own-repo fixes

96 findings on the tool's own code, all fixed (no baseline): 48 in
`fix_engine.py`, 41 in `scanner/`, 6 in `lucidlint.py`, 1 in `scripts/`.
Each became a named predicate or a named local; the scanner's own token
mappers and `__eq__` implementations are exempt by the rule's own shape.

## Gates

`make check`, `make test` (316 pytest + 461 Rust), `make self-check` at the
same 2 pre-existing `layer-mix` failures as `b48530a` (both unrelated to this
rule), `make coverage` green.
