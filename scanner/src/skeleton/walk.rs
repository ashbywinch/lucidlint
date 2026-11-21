//! The skeleton walk: one exhaustive mapping from the AST's nodes to their
//! structural tokens.
// lucidlint: ignore-file large-function the skeleton builder is ONE exhaustive dispatch
// table — splitting it turns one mapping into a hunt through several functions

use ruff_python_ast::{AnyNodeRef, BoolOp, CmpOp, ExprContext, Operator, Pattern, StmtFunctionDef, UnaryOp};

use super::Q;

/// The skeleton walk — the token builders and the BFS over the AST's children.
pub struct SkeletonWalk;

impl SkeletonWalk {
    pub fn fn_skeleton(f: &StmtFunctionDef) -> Vec<String> {
        use ruff_python_ast::AnyNodeRef;
        let mut toks: Vec<String> = Vec::new();
        let mut queue: Vec<Q> = Vec::new();
        toks.push(if f.is_async { "AsyncFunctionDef" } else { "FunctionDef" }.to_string());
        // CPython FunctionDef children: arguments, body, decorator_list, returns
        queue.push(Q::N(AnyNodeRef::Parameters(&f.parameters)));
        for s in &f.body {
            queue.push(Q::N(AnyNodeRef::from(s)));
        }
        for d in &f.decorator_list {
            queue.push(Q::N(AnyNodeRef::from(&d.expression)));
        }
        if let Some(r) = &f.returns {
            queue.push(Q::N(AnyNodeRef::from(r.as_ref())));
        }
        let mut i = 0usize;
        while i < queue.len() {
            let node = match &queue[i] {
                Q::N(n) => *n,
                Q::T(t) => {
                    toks.push((*t).to_string());
                    i += 1;
                    continue;
                }
            };
            i += 1;
            match node {
                AnyNodeRef::ExprName(_) => toks.push("N".to_string()),
                AnyNodeRef::ExprStringLiteral(_)
                | AnyNodeRef::ExprBytesLiteral(_)
                | AnyNodeRef::ExprNumberLiteral(_)
                | AnyNodeRef::ExprBooleanLiteral(_)
                | AnyNodeRef::ExprNoneLiteral(_)
                | AnyNodeRef::ExprEllipsisLiteral(_)
                | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push("C".to_string()),
                AnyNodeRef::Parameter(_) | AnyNodeRef::ParameterWithDefault(_) => toks.push("A".to_string()),
                AnyNodeRef::Parameters(_) => toks.push("arguments".to_string()),
                AnyNodeRef::ElifElseClause(_) => toks.push("If".to_string()),
                AnyNodeRef::InterpolatedElement(_) => toks.push("FormattedValue".to_string()),
                _ => toks.push(format!("{:?}", node.kind())),
            }
            Self::skel_children(node, &mut queue);
        }
        toks
    }
    fn op_token(op: &Operator) -> &'static str {
        use ruff_python_ast::Operator as O;
        match op {
            O::Add => "Add",
            O::Sub => "Sub",
            O::Mult => "Mult",
            O::MatMult => "MatMult",
            O::Div => "Div",
            O::Mod => "Mod",
            O::Pow => "Pow",
            O::LShift => "LShift",
            O::RShift => "RShift",
            O::BitOr => "BitOr",
            O::BitXor => "BitXor",
            O::BitAnd => "BitAnd",
            O::FloorDiv => "FloorDiv",
        }
    }
    fn unary_op_token(op: &UnaryOp) -> &'static str {
        use ruff_python_ast::UnaryOp as U;
        match op {
            U::Invert => "Invert",
            U::Not => "Not",
            U::UAdd => "UAdd",
            U::USub => "USub",
        }
    }
    fn bool_op_token(op: &BoolOp) -> &'static str {
        use ruff_python_ast::BoolOp as B;
        match op {
            B::And => "And",
            B::Or => "Or",
        }
    }
    fn cmp_op_token(op: &CmpOp) -> &'static str {
        use ruff_python_ast::CmpOp as C;
        match op {
            C::Eq => "Eq",
            C::NotEq => "NotEq",
            C::Lt => "Lt",
            C::LtE => "LtE",
            C::Gt => "Gt",
            C::GtE => "GtE",
            C::Is => "Is",
            C::IsNot => "IsNot",
            C::In => "In",
            C::NotIn => "NotIn",
        }
    }
    fn ctx_token(ctx: &ExprContext) -> &'static str {
        use ruff_python_ast::ExprContext as X;
        match ctx {
            X::Load => "Load",
            X::Store => "Store",
            X::Del => "Del",
            X::Invalid => "Invalid",
        }
    }
    /// Push a pattern node onto the skeleton queue.
    fn push_pattern<'a>(queue: &mut Vec<Q<'a>>, p: &'a Pattern) {
        let n = match p {
            Pattern::MatchValue(x) => AnyNodeRef::PatternMatchValue(x),
            Pattern::MatchSingleton(x) => AnyNodeRef::PatternMatchSingleton(x),
            Pattern::MatchSequence(x) => AnyNodeRef::PatternMatchSequence(x),
            Pattern::MatchMapping(x) => AnyNodeRef::PatternMatchMapping(x),
            Pattern::MatchClass(x) => AnyNodeRef::PatternMatchClass(x),
            Pattern::MatchStar(x) => AnyNodeRef::PatternMatchStar(x),
            Pattern::MatchAs(x) => AnyNodeRef::PatternMatchAs(x),
            Pattern::MatchOr(x) => AnyNodeRef::PatternMatchOr(x),
        };
        queue.push(Q::N(n));
    }
    /// One BFS queue item — the walker's two push helpers (module-level: a
    /// nested pair would hide structure the latent-class rule exists to surface).
    fn skel_push<'a>(q: &mut Vec<Q<'a>>, n: AnyNodeRef<'a>) {
        q.push(Q::N(n));
    }
    fn skel_tok<'a>(q: &mut Vec<Q<'a>>, t: &'static str) {
        q.push(Q::T(t));
    }
    pub fn skel_children<'a>(node: AnyNodeRef<'a>, queue: &mut Vec<Q<'a>>) {
        use ruff_python_ast::Pattern;
        match node {
        AnyNodeRef::StmtClassDef(c) => {
            // CPython: bases, keywords, body, decorator_list
            if let Some(arguments) = &c.arguments {
                for b in &arguments.args {
                    Self::skel_push(queue, AnyNodeRef::from(b));
                }
                for k in &arguments.keywords {
                    Self::skel_push(queue, AnyNodeRef::Keyword(k));
                }
            }
            for s in &c.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &c.decorator_list {
                Self::skel_push(queue, AnyNodeRef::from(&d.expression));
            }
        }
        AnyNodeRef::StmtReturn(r) => {
            if let Some(v) = &r.value {
                Self::skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtDelete(d) => {
            for t in &d.targets {
                Self::skel_push(queue, AnyNodeRef::from(t));
            }
        }
        AnyNodeRef::StmtTypeAlias(t) => {
            Self::skel_push(queue, AnyNodeRef::from(t.name.as_ref()));
            if let Some(tp) = &t.type_params {
                for p in &tp.type_params {
                    match p {
                        ruff_python_ast::TypeParam::TypeVar(t) => Self::skel_push(queue, AnyNodeRef::TypeParamTypeVar(t)),
                        ruff_python_ast::TypeParam::TypeVarTuple(t) => Self::skel_push(queue, AnyNodeRef::TypeParamTypeVarTuple(t)),
                        ruff_python_ast::TypeParam::ParamSpec(t) => Self::skel_push(queue, AnyNodeRef::TypeParamParamSpec(t)),
                    }
                }
            }
            Self::skel_push(queue, AnyNodeRef::from(t.value.as_ref()));
        }
        AnyNodeRef::StmtAssign(a) => {
            for t in &a.targets {
                Self::skel_push(queue, AnyNodeRef::from(t));
            }
            Self::skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAugAssign(a) => {
            Self::skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            Self::skel_tok(queue, Self::op_token(&a.op));
            Self::skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAnnAssign(a) => {
            Self::skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(a.annotation.as_ref()));
            if let Some(v) = &a.value {
                Self::skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtFor(f) => {
            Self::skel_push(queue, AnyNodeRef::from(f.target.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(f.iter.as_ref()));
            for s in &f.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &f.orelse {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWhile(w) => {
            Self::skel_push(queue, AnyNodeRef::from(w.test.as_ref()));
            for s in &w.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &w.orelse {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtIf(i) => {
            Self::skel_push(queue, AnyNodeRef::from(i.test.as_ref()));
            for s in &i.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            // CPython re-nests elifs as inner If nodes in orelse
            for clause in &i.elif_else_clauses {
                Self::skel_push(queue, AnyNodeRef::ElifElseClause(clause));
            }
        }
        AnyNodeRef::ElifElseClause(clause) => {
            if let Some(t) = &clause.test {
                Self::skel_push(queue, AnyNodeRef::from(t));
            }
            for s in &clause.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWith(w) => {
            for item in &w.items {
                Self::skel_push(queue, AnyNodeRef::WithItem(item));
            }
            for s in &w.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::WithItem(item) => {
            Self::skel_push(queue, AnyNodeRef::from(&item.context_expr));
            if let Some(v) = &item.optional_vars {
                Self::skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtMatch(m) => {
            Self::skel_push(queue, AnyNodeRef::from(m.subject.as_ref()));
            for case in &m.cases {
                Self::skel_push(queue, AnyNodeRef::MatchCase(case));
            }
        }
        AnyNodeRef::MatchCase(case) => {
            match &case.pattern {
                Pattern::MatchValue(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchValue(p)),
                Pattern::MatchSingleton(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchSingleton(p)),
                Pattern::MatchSequence(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchSequence(p)),
                Pattern::MatchMapping(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchMapping(p)),
                Pattern::MatchClass(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchClass(p)),
                Pattern::MatchStar(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchStar(p)),
                Pattern::MatchAs(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchAs(p)),
                Pattern::MatchOr(p) => Self::skel_push(queue, AnyNodeRef::PatternMatchOr(p)),
            }
            if let Some(g) = &case.guard {
                Self::skel_push(queue, AnyNodeRef::from(g.as_ref()));
            }
            for s in &case.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtRaise(r) => {
            if let Some(e) = &r.exc {
                Self::skel_push(queue, AnyNodeRef::from(e.as_ref()));
            }
            if let Some(c) = &r.cause {
                Self::skel_push(queue, AnyNodeRef::from(c.as_ref()));
            }
        }
        AnyNodeRef::StmtTry(t) => {
            for s in &t.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                Self::skel_push(queue, AnyNodeRef::ExceptHandlerExceptHandler(eh));
            }
            for s in &t.orelse {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &t.finalbody {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::ExceptHandlerExceptHandler(eh) => {
            if let Some(t) = &eh.type_ {
                Self::skel_push(queue, AnyNodeRef::from(t.as_ref()));
            }
            for s in &eh.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtAssert(a) => {
            Self::skel_push(queue, AnyNodeRef::from(a.test.as_ref()));
            if let Some(m) = &a.msg {
                Self::skel_push(queue, AnyNodeRef::from(m.as_ref()));
            }
        }
        AnyNodeRef::StmtImport(imp) => {
            for a in &imp.names {
                Self::skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtImportFrom(imp) => {
            for a in &imp.names {
                Self::skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtExpr(e) => Self::skel_push(queue, AnyNodeRef::from(e.value.as_ref())),
        // a nested function's subtree IS part of ast.walk — descend like the
        // module-level root: arguments, body, decorator_list, returns
        AnyNodeRef::StmtFunctionDef(f) => {
            Self::skel_push(queue, AnyNodeRef::Parameters(&f.parameters));
            for s in &f.body {
                Self::skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &f.decorator_list {
                Self::skel_push(queue, AnyNodeRef::from(&d.expression));
            }
            if let Some(r) = &f.returns {
                Self::skel_push(queue, AnyNodeRef::from(r.as_ref()));
            }
        }
        // leaf / identifier-only statements
        AnyNodeRef::StmtGlobal(_)
        | AnyNodeRef::StmtNonlocal(_)
        | AnyNodeRef::StmtPass(_)
        | AnyNodeRef::StmtBreak(_)
        | AnyNodeRef::StmtContinue(_)
        | AnyNodeRef::StmtIpyEscapeCommand(_)
        | AnyNodeRef::Alias(_) => {}
        AnyNodeRef::Parameters(p) => {
            // CPython arguments children: posonlyargs, args, vararg,
            // kwonlyargs, kw_defaults, kwarg, defaults (hoisted)
            for pwd in &p.posonlyargs {
                Self::skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in &p.args {
                Self::skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            if let Some(v) = &p.vararg {
                Self::skel_push(queue, AnyNodeRef::Parameter(v));
            }
            for pwd in &p.kwonlyargs {
                Self::skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in p.kwonlyargs.iter().filter_map(|p| p.default.as_deref()) {
                Self::skel_push(queue, AnyNodeRef::from(pwd));
            }
            if let Some(k) = &p.kwarg {
                Self::skel_push(queue, AnyNodeRef::Parameter(k));
            }
            for pwd in p
                .posonlyargs
                .iter()
                .chain(&p.args)
                .filter_map(|p| p.default.as_deref())
            {
                Self::skel_push(queue, AnyNodeRef::from(pwd));
            }
        }
        AnyNodeRef::Parameter(param) => {
            if let Some(a) = &param.annotation {
                Self::skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::ParameterWithDefault(pwd) => {
            // the default is hoisted by Parameters (CPython defaults list)
            if let Some(a) = &pwd.parameter.annotation {
                Self::skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::Keyword(k) => Self::skel_push(queue, AnyNodeRef::from(&k.value)),
        AnyNodeRef::Comprehension(c) => {
            Self::skel_push(queue, AnyNodeRef::from(&c.target));
            Self::skel_push(queue, AnyNodeRef::from(&c.iter));
            for f in &c.ifs {
                Self::skel_push(queue, AnyNodeRef::from(f));
            }
        }
        AnyNodeRef::ExprBoolOp(b) => {
            Self::skel_tok(queue, Self::bool_op_token(&b.op));
            for v in &b.values {
                Self::skel_push(queue, AnyNodeRef::from(v));
            }
        }
        AnyNodeRef::ExprNamed(n) => {
            Self::skel_push(queue, AnyNodeRef::from(n.target.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(n.value.as_ref()));
        }
        AnyNodeRef::ExprBinOp(b) => {
            Self::skel_push(queue, AnyNodeRef::from(b.left.as_ref()));
            Self::skel_tok(queue, Self::op_token(&b.op));
            Self::skel_push(queue, AnyNodeRef::from(b.right.as_ref()));
        }
        AnyNodeRef::ExprUnaryOp(u) => {
            Self::skel_tok(queue, Self::unary_op_token(&u.op));
            Self::skel_push(queue, AnyNodeRef::from(u.operand.as_ref()));
        }
        AnyNodeRef::ExprLambda(l) => {
            if let Some(p) = &l.parameters {
                Self::skel_push(queue, AnyNodeRef::Parameters(p));
            }
            Self::skel_push(queue, AnyNodeRef::from(l.body.as_ref()));
        }
        AnyNodeRef::ExprIf(e) => {
            Self::skel_push(queue, AnyNodeRef::from(e.test.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(e.body.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(e.orelse.as_ref()));
        }
        AnyNodeRef::ExprDict(d) => {
            // CPython: all keys first, then all values
            for item in &d.items {
                if let Some(k) = &item.key {
                    Self::skel_push(queue, AnyNodeRef::from(k));
                }
            }
            for item in &d.items {
                Self::skel_push(queue, AnyNodeRef::from(&item.value));
            }
        }
        AnyNodeRef::ExprSet(s) => {
            for e in &s.elts {
                Self::skel_push(queue, AnyNodeRef::from(e));
            }
        }
        AnyNodeRef::ExprListComp(c) => {
            Self::skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                Self::skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprSetComp(c) => {
            Self::skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                Self::skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprGenerator(g) => {
            Self::skel_push(queue, AnyNodeRef::from(g.elt.as_ref()));
            for gen in &g.generators {
                Self::skel_push(queue, AnyNodeRef::Comprehension(gen));
            }
        }
        AnyNodeRef::ExprDictComp(c) => {
            if let Some(k) = &c.key {
                Self::skel_push(queue, AnyNodeRef::from(k.as_ref()));
            }
            Self::skel_push(queue, AnyNodeRef::from(c.value.as_ref()));
            for g in &c.generators {
                Self::skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprAwait(a) => Self::skel_push(queue, AnyNodeRef::from(a.value.as_ref())),
        AnyNodeRef::ExprYield(y) => {
            if let Some(v) = &y.value {
                Self::skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::ExprYieldFrom(y) => Self::skel_push(queue, AnyNodeRef::from(y.value.as_ref())),
        AnyNodeRef::ExprCompare(c) => {
            Self::skel_push(queue, AnyNodeRef::from(c.left.as_ref()));
            for o in &c.ops {
                Self::skel_tok(queue, Self::cmp_op_token(o));
            }
            for o in &c.comparators {
                Self::skel_push(queue, AnyNodeRef::from(o));
            }
        }
        AnyNodeRef::ExprCall(c) => {
            Self::skel_push(queue, AnyNodeRef::from(c.func.as_ref()));
            for a in &c.arguments.args {
                Self::skel_push(queue, AnyNodeRef::from(a));
            }
            for k in &c.arguments.keywords {
                Self::skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        AnyNodeRef::ExprFString(f) => {
            for element in f.value.elements() {
                Self::skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::ExprTString(t) => {
            // t-strings (3.14) — structurally a JoinedStr; walk parts' elements
            for element in t.value.elements() {
                Self::skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::InterpolatedElement(e) => {
            Self::skel_push(queue, AnyNodeRef::from(e.expression.as_ref()));
            if let Some(spec) = &e.format_spec {
                for element in &spec.elements {
                    Self::skel_push(queue, AnyNodeRef::from(element));
                }
            }
        }
        AnyNodeRef::InterpolatedStringFormatSpec(_) | AnyNodeRef::InterpolatedStringLiteralElement(_) => {}
        AnyNodeRef::ExprAttribute(a) => {
            Self::skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
            Self::skel_tok(queue, Self::ctx_token(&a.ctx));
        }
        AnyNodeRef::ExprSubscript(s) => {
            Self::skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            Self::skel_push(queue, AnyNodeRef::from(s.slice.as_ref()));
            Self::skel_tok(queue, Self::ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprStarred(s) => {
            Self::skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            Self::skel_tok(queue, Self::ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprList(l) => {
            for e in &l.elts {
                Self::skel_push(queue, AnyNodeRef::from(e));
            }
            Self::skel_tok(queue, Self::ctx_token(&l.ctx));
        }
        AnyNodeRef::ExprTuple(t) => {
            for e in &t.elts {
                Self::skel_push(queue, AnyNodeRef::from(e));
            }
            Self::skel_tok(queue, Self::ctx_token(&t.ctx));
        }
        AnyNodeRef::ExprSlice(s) => {
            if let Some(l) = &s.lower {
                Self::skel_push(queue, AnyNodeRef::from(l.as_ref()));
            }
            if let Some(u) = &s.upper {
                Self::skel_push(queue, AnyNodeRef::from(u.as_ref()));
            }
            if let Some(st) = &s.step {
                Self::skel_push(queue, AnyNodeRef::from(st.as_ref()));
            }
        }
        AnyNodeRef::PatternMatchValue(p) => Self::skel_push(queue, AnyNodeRef::from(p.value.as_ref())),
        AnyNodeRef::PatternMatchSequence(p) => {
            for pat in &p.patterns {
                Self::push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchMapping(p) => {
            for k in &p.keys {
                Self::skel_push(queue, AnyNodeRef::from(k));
            }
            for pat in &p.patterns {
                Self::push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchClass(p) => {
            Self::skel_push(queue, AnyNodeRef::from(p.cls.as_ref()));
            for pat in &p.arguments.patterns {
                Self::push_pattern(queue, pat);
            }
            for kw in &p.arguments.keywords {
                Self::push_pattern(queue, &kw.pattern);
            }
        }
        AnyNodeRef::PatternMatchAs(p) => {
            if let Some(pat) = &p.pattern {
                Self::push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchOr(p) => {
            for pat in &p.patterns {
                Self::push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchSingleton(_) | AnyNodeRef::PatternMatchStar(_) => {}
        AnyNodeRef::TypeParamTypeVar(t) => {
            if let Some(b) = &t.bound {
                Self::skel_push(queue, AnyNodeRef::from(b.as_ref()));
            }
            if let Some(d) = &t.default {
                Self::skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamTypeVarTuple(t) => {
            if let Some(d) = &t.default {
                Self::skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamParamSpec(t) => {
            if let Some(d) = &t.default {
                Self::skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::PatternArguments(_) | AnyNodeRef::PatternKeyword(_) => {}
        AnyNodeRef::Arguments(a) => {
            for e in &a.args {
                Self::skel_push(queue, AnyNodeRef::from(e));
            }
            for k in &a.keywords {
                Self::skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        // literals, names, module — leaves
        AnyNodeRef::ExprName(n) => Self::skel_tok(queue, Self::ctx_token(&n.ctx)),
        AnyNodeRef::ExprStringLiteral(_)
        | AnyNodeRef::ExprBytesLiteral(_)
        | AnyNodeRef::ExprNumberLiteral(_)
        | AnyNodeRef::ExprBooleanLiteral(_)
        | AnyNodeRef::ExprNoneLiteral(_)
        | AnyNodeRef::ExprEllipsisLiteral(_)
        | AnyNodeRef::ExprIpyEscapeCommand(_)
        | AnyNodeRef::ModModule(_)
        | AnyNodeRef::ModExpression(_)
        // never pushed by skel_children, but the match must be total
        | AnyNodeRef::Decorator(_)
        | AnyNodeRef::TypeParams(_)
        | AnyNodeRef::FString(_)
        | AnyNodeRef::TString(_)
        | AnyNodeRef::StringLiteral(_)
        | AnyNodeRef::BytesLiteral(_)
        | AnyNodeRef::Identifier(_) => {}
    }
    }
}
