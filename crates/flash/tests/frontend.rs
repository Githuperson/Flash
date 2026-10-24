//! Lexer, parser, name resolution, type inference, ownership and IR/optimiser tests using the
//! compiler library directly.

use flashc::ast::*;
use flashc::diag::Diagnostics;
use flashc::driver::frontend_from_source;
use flashc::hir::*;
use flashc::lexer::{lex, FPart, Tok};
use flashc::types::*;

fn toks(src: &str) -> (Vec<Tok>, Diagnostics) {
    let mut d = Diagnostics::default();
    let t = lex(src, 0, &mut d).into_iter().map(|t| t.tok).collect();
    (t, d)
}

fn parse(src: &str) -> (Module, Diagnostics) {
    let mut d = Diagnostics::default();
    let m = flashc::parser::parse_module(src, 0, &mut d);
    (m, d)
}

fn errors_of(src: &str) -> String {
    match frontend_from_source("t.flash", src) {
        Ok(_) => String::new(),
        Err(e) => e,
    }
}

fn program(src: &str) -> Program {
    match frontend_from_source("t.flash", src) {
        Ok(f) => f.program,
        Err(e) => panic!("unexpected compile error:\n{}", e),
    }
}

// ----------------------------------------------------------------- lexer

#[test]
fn lexer_emits_indent_and_dedent_tokens() {
    let (t, d) = toks("if x:\n    a = 1\n    if y:\n        b = 2\nc = 3\n");
    assert!(!d.has_errors());
    let indents = t.iter().filter(|x| **x == Tok::Indent).count();
    let dedents = t.iter().filter(|x| **x == Tok::Dedent).count();
    assert_eq!((indents, dedents), (2, 2));
}

#[test]
fn lexer_ignores_blank_lines_and_comments_when_indenting() {
    let (t, d) = toks("if x:\n\n    # note\n        # deeper comment\n    a = 1\n\n# top comment\nb = 2\n");
    assert!(!d.has_errors());
    assert_eq!(t.iter().filter(|x| **x == Tok::Indent).count(), 1);
    assert_eq!(t.iter().filter(|x| **x == Tok::Dedent).count(), 1);
}

#[test]
fn lexer_rejects_tabs_and_bad_dedent() {
    let (_, d) = toks("if x:\n\ta = 1\n");
    assert!(d.items.iter().any(|x| x.message.contains("Tabs are not allowed")));
    let (_, d) = toks("if x:\n        a = 1\n    b = 2\n");
    assert!(d.items.iter().any(|x| x.message.contains("indented differently")));
}

#[test]
fn lexer_joins_lines_inside_brackets() {
    let (t, d) = toks("x = [1,\n  2,\n  3]\ny = 4\n");
    assert!(!d.has_errors());
    assert_eq!(t.iter().filter(|x| **x == Tok::Newline).count(), 2);
    assert!(!t.contains(&Tok::Indent));
}

#[test]
fn lexer_numbers_ranges_and_strings() {
    let (t, d) = toks("a = 0..10\nb = 1.5e3\nc = 0xFF\nd = 1_000\ne = \"x\\n\\\"q\\\"\"\nf = 3..=4\n");
    assert!(!d.has_errors());
    assert!(t.contains(&Tok::DotDot) && t.contains(&Tok::DotDotEq));
    assert!(t.contains(&Tok::Float(1500.0)));
    assert!(t.contains(&Tok::Int(255)) && t.contains(&Tok::Int(1000)));
    assert!(t.contains(&Tok::Str("x\n\"q\"".into())));
    // `0..10` is two ints and a range operator, not a float
    assert!(t.windows(3).any(|w| w == [Tok::Int(0), Tok::DotDot, Tok::Int(10)]));
}

#[test]
fn lexer_fstring_parts_and_unterminated_string() {
    let (t, d) = toks("s = f\"a{x + 1}b{{c}}\"\n");
    assert!(!d.has_errors());
    let parts = t.iter().find_map(|x| if let Tok::FStr(p) = x { Some(p.clone()) } else { None }).unwrap();
    assert_eq!(parts.len(), 3);
    assert!(matches!(&parts[0], FPart::Lit(s) if s == "a"));
    assert!(matches!(&parts[1], FPart::Expr(s, _) if s == "x + 1"));
    assert!(matches!(&parts[2], FPart::Lit(s) if s == "b{c}"));
    let (_, d) = toks("s = \"oops\n");
    assert!(d.items.iter().any(|x| x.message.contains("missing its closing quote")));
}

// ---------------------------------------------------------------- parser

fn expr_of(src: &str) -> Expr {
    let (m, d) = parse(&format!("x = {}\n", src));
    assert!(!d.has_errors(), "{:?}", d.items);
    match &m.items[0] {
        Item::Stmt(Stmt { kind: StmtKind::Assign { value, .. }, .. }) => value.clone(),
        other => panic!("{:?}", other),
    }
}

#[test]
fn parser_respects_operator_precedence() {
    // 1 + 2 * 3  =>  1 + (2 * 3)
    match expr_of("1 + 2 * 3").kind {
        ExprKind::Binary(BinOp::Add, l, r) => {
            assert!(matches!(l.kind, ExprKind::Int(1)));
            assert!(matches!(r.kind, ExprKind::Binary(BinOp::Mul, _, _)));
        }
        k => panic!("{:?}", k),
    }
    // a or b and c  =>  a or (b and c)
    match expr_of("a or b and c").kind {
        ExprKind::Binary(BinOp::Or, _, r) => assert!(matches!(r.kind, ExprKind::Binary(BinOp::And, _, _))),
        k => panic!("{:?}", k),
    }
    // not a == b  =>  not (a == b)
    assert!(matches!(expr_of("not a == b").kind, ExprKind::Unary(UnOp::Not, _)));
    // -2 is a literal; -x is a unary op; comparisons bind looser than arithmetic
    assert!(matches!(expr_of("-2").kind, ExprKind::Int(-2)));
    assert!(matches!(expr_of("-x").kind, ExprKind::Unary(UnOp::Neg, _)));
    assert!(matches!(expr_of("a + 1 < b * 2").kind, ExprKind::Binary(BinOp::Lt, _, _)));
    // postfix chain
    assert!(matches!(expr_of("a.b(1)[2]?").kind, ExprKind::Try(_)));
}

#[test]
fn parser_builds_functions_structs_and_control_flow() {
    let src = "struct P:\n    name: String\n    n: Int\n\nfn f(a, b: Int, mut c: List[Int], own d) -> Int:\n    for i in 0..=3:\n        pass\n    if a:\n        return 1\n    elif b:\n        return 2\n    else:\n        return 3\n";
    let (m, d) = parse(src);
    assert!(!d.has_errors(), "{:?}", d.items);
    let Item::Struct(s) = &m.items[0] else { panic!() };
    assert_eq!(s.fields.len(), 2);
    let Item::Fn(f) = &m.items[1] else { panic!() };
    assert_eq!(f.params.len(), 4);
    assert_eq!(f.params[2].mode, ParamMode::Mut);
    assert_eq!(f.params[3].mode, ParamMode::Own);
    assert!(f.params[0].ty.is_none() && f.params[1].ty.is_some());
    assert_eq!(f.ret.as_ref().unwrap().name, "Int");
    assert!(matches!(&f.body[0].kind, StmtKind::For { iter: ForIter::Range { inclusive: true, .. }, .. }));
    let StmtKind::If { branches, else_body } = &f.body[1].kind else { panic!() };
    assert_eq!(branches.len(), 2);
    assert!(else_body.is_some());
}

#[test]
fn parser_reports_several_errors_and_recovers() {
    let (m, d) = parse("x = 1 +\ny = 2\nif True\n    say(1)\nz = 3\nw = @\n");
    assert!(d.error_count() >= 3, "{:?}", d.items.iter().map(|x| &x.message).collect::<Vec<_>>());
    // statements after the errors are still parsed
    assert!(m.items.iter().any(|i| matches!(i, Item::Stmt(Stmt { kind: StmtKind::Assign { .. }, .. }))));
}

#[test]
fn parser_error_messages_are_helpful() {
    let (_, d) = parse("if True:\n    say(1)\nelse if False:\n    say(2)\n");
    assert!(d.items.iter().any(|x| x.message.contains("`elif`")));
    let (_, d) = parse("fn f():\npass\n");
    assert!(d.items.iter().any(|x| x.message.contains("indented block")));
    let (_, d) = parse("x = 1 < 2 < 3\n");
    assert!(d.items.iter().any(|x| x.message.contains("chained")));
    let (_, d) = parse("use rust_plugin fast_math\n");
    assert!(d.has_errors());
}

// ------------------------------------------------------------- resolution

#[test]
fn variables_are_block_scoped() {
    assert!(errors_of("if True:\n    inner = 1\nsay(inner)\n").contains("Unknown variable 'inner'"));
    assert!(errors_of("for i in 0..3:\n    pass\nsay(i)\n").contains("Unknown variable 'i'"));
    // a variable assigned before the block is visible and assignable inside it
    assert_eq!(errors_of("x = 1\nif True:\n    x = 2\nsay(x)\n"), "");
}

#[test]
fn functions_do_not_see_script_variables() {
    let e = errors_of("limit = 10\nfn f():\n    return limit\nsay(f())\n");
    assert!(e.contains("Unknown variable 'limit'"), "{}", e);
}

#[test]
fn functions_can_be_used_before_they_are_defined() {
    assert_eq!(errors_of("say(twice(4))\nfn twice(x):\n    return x * 2\n"), "");
}

#[test]
fn unused_generic_function_bodies_are_still_name_checked() {
    let e = errors_of("fn never_called(x):\n    return x + missing\n");
    assert!(e.contains("Unknown variable 'missing'"), "{}", e);
}

// --------------------------------------------------------------- inference

fn ret_type(p: &Program, name: &str) -> Vec<Type> {
    p.funcs.iter().filter(|f| f.name == name).map(|f| f.ret.clone()).collect()
}

#[test]
fn return_and_parameter_types_are_inferred_per_call() {
    let p = program("fn add(a, b):\n    return a + b\nsay(add(1, 2))\nsay(add(1.5, 2.5))\nsay(add(\"a\", \"b\"))\n");
    let mut rets = ret_type(&p, "add");
    rets.sort_by_key(|t| format!("{:?}", t));
    assert_eq!(rets, vec![Type::Float, Type::Int(INT), Type::Str]);
    // identical argument types share one instance
    let p = program("fn id(a):\n    return a\nsay(id(1))\nsay(id(2))\n");
    assert_eq!(ret_type(&p, "id").len(), 1);
}

#[test]
fn recursion_return_type_is_inferred_from_the_base_case() {
    let p = program("fn fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nsay(fib(10))\n");
    assert_eq!(ret_type(&p, "fib"), vec![Type::Int(INT)]);
    let e = errors_of("fn bad(n):\n    return bad(n - 1)\nsay(bad(3))\n");
    assert!(e.contains("Add a return type"), "{}", e);
}

#[test]
fn variables_infer_types_and_annotations_are_checked() {
    assert_eq!(errors_of("a = 1\nb = 2.5\nc = True\nd = \"s\"\ne = None\nf = [1, 2]\n"), "");
    assert!(errors_of("a: Float = 1\n").contains("should be Float, but this is Int"));
    assert!(errors_of("a: Int = 1\na = 2.5\n").contains("should be Int, but this is Float"));
    // literals adapt to the annotated or neighbouring integer type
    assert_eq!(errors_of("a: u8 = 200\nb = a + 55\nc: i32 = 5\nd = 1 + c\n"), "");
}

#[test]
fn generic_errors_name_the_call_site() {
    let e = errors_of("fn bad(x):\n    return x * 2\nsay(bad(1.5))\n");
    assert!(e.contains("Cannot multiply Float and Int."), "{}", e);
    assert!(e.contains("while checking 'bad' with argument types (Float)"), "{}", e);
}

#[test]
fn struct_construction_and_field_access_are_typed() {
    let p = program("struct P:\n    name: String\n    n: Int\np = P(n=1, name=\"a\")\nsay(p.n + 1)\n");
    assert_eq!(p.structs[0].fields[0].1, Type::Str);
    assert!(errors_of("struct P:\n    n: Int\np = P(n=\"x\")\n").contains("The field 'n' should be Int"));
    assert!(errors_of("struct P:\n    n: Int\np = P(n=1, m=2)\n").contains("no parameter named 'm'"));
    // positional construction works too
    assert_eq!(errors_of("struct P:\n    a: Int\n    b: Int\np = P(1, 2)\nsay(p.a + p.b)\n"), "");
}

// --------------------------------------------------------------- ownership

fn first_use_modes(p: &Program, local: &str) -> Vec<UseMode> {
    fn walk(e: &HExpr, f: &HFunc, name: &str, out: &mut Vec<UseMode>) {
        match &e.kind {
            HKind::Local(id, m) if f.locals[*id].name == name => out.push(*m),
            HKind::Call(_, a) | HKind::Builtin(_, a) | HKind::Struct(a) | HKind::List(a) => a.iter().for_each(|x| walk(x, f, name, out)),
            HKind::Binary(_, a, b) => {
                walk(a, f, name, out);
                walk(b, f, name, out)
            }
            HKind::Field(a, _) | HKind::Cast(a) | HKind::Unary(_, a) => walk(a, f, name, out),
            _ => {}
        }
    }
    let main = &p.funcs[p.main];
    let mut out = vec![];
    for s in &main.body {
        match &s.kind {
            HStmtKind::Let(_, e) | HStmtKind::Expr(e) => walk(e, main, local, &mut out),
            HStmtKind::Assign(_, e) => walk(e, main, local, &mut out),
            _ => {}
        }
    }
    out
}

#[test]
fn copy_types_copy_and_owned_types_move() {
    let p = program("n = 5\nm = n\nk = n + 1\ns = \"a\"\nt = s\n");
    assert_eq!(first_use_modes(&p, "n"), vec![UseMode::Copy, UseMode::Copy]);
    assert_eq!(first_use_modes(&p, "s"), vec![UseMode::Move]);
}

#[test]
fn borrowed_arguments_do_not_move() {
    let p = program("fn show(s: String):\n    say(s)\nx = \"hi\"\nshow(x)\nshow(x)\nsay(x)\n");
    assert!(first_use_modes(&p, "x").iter().all(|m| *m == UseMode::Borrow));
    let e = errors_of("fn eat(own s: String):\n    say(s)\nx = \"hi\"\neat(x)\nsay(x)\n");
    assert!(e.contains("was moved earlier"), "{}", e);
}

#[test]
fn ownership_rules_reject_unsound_programs() {
    // use after move through a struct field initialiser
    assert!(errors_of("struct B:\n    s: String\nx = \"a\"\nb = B(s=x)\nsay(x)\n").contains("moved earlier"));
    // conditional move then use
    assert!(errors_of("x = \"a\"\nif True:\n    y = x\nsay(x)\n").contains("moved earlier"));
    // moving in one branch and returning in the other is fine
    assert_eq!(errors_of("fn f(own x: String, c: Bool) -> String:\n    if c:\n        return x\n    return x\nsay(f(\"a\", True))\n"), "");
    // reinitialisation after a move is fine
    assert_eq!(errors_of("x = \"a\"\ny = x\nx = \"b\"\nsay(x, y)\n"), "");
    // aliasing exclusive access in one call
    assert!(errors_of("fn f(mut a: List[Int], b: List[Int]):\n    append(a, len(b))\nxs = [1]\nf(xs, xs)\n").contains("more than once"));
    // a borrowed parameter cannot be returned
    assert!(errors_of("fn f(s: String) -> String:\n    return s\nsay(f(\"a\"))\n").contains("only borrowed"));
    // list element cannot be moved out
    assert!(errors_of("xs = [\"a\"]\ny = xs[0]\n").contains("out of its container"));
}

#[test]
fn loops_detect_moves_in_previous_iterations() {
    let e = errors_of("fn eat(own s: String):\n    say(s)\nx = \"a\"\nwhile True:\n    eat(x)\n");
    assert!(e.contains("was moved earlier"), "{}", e);
    // moving a fresh value each iteration is fine
    assert_eq!(errors_of("fn eat(own s: String):\n    say(s)\nfor i in 0..3:\n    x = \"a\"\n    eat(x)\n"), "");
    // moved inside a loop but the loop always breaks right after
    assert_eq!(errors_of("fn eat(own s: String):\n    say(s)\nx = \"a\"\nfor i in 0..3:\n    eat(x)\n    break\n"), "");
}

// ------------------------------------------------------------ IR / optimiser

fn lowered(src: &str, release: bool) -> flashc::ir::Module {
    let p = program(src);
    flashc::driver::lower_and_optimise(&p, release)
}

fn main_fn(m: &flashc::ir::Module) -> &flashc::ir::Function {
    m.funcs.iter().find(|f| f.name == "flash_main").unwrap()
}

fn count_insts(f: &flashc::ir::Function, pred: impl Fn(&flashc::ir::Inst) -> bool) -> usize {
    f.blocks.iter().flat_map(|b| b.insts.iter()).filter(|i| pred(i)).count()
}

#[test]
fn constant_folding_removes_arithmetic() {
    use flashc::ir::*;
    let m = lowered("x = 2 + 3 * 4\ny = x * 2 - 4\nsay(y)\n", false);
    let f = main_fn(&m);
    assert_eq!(count_insts(f, |i| matches!(i, Inst::Bin { .. })), 0, "{}", m);
    assert!(count_insts(f, |i| matches!(i, Inst::IConst { val: 24, .. })) >= 1, "{}", m);
}

#[test]
fn constant_conditions_fold_away_dead_branches() {
    use flashc::ir::*;
    let m = lowered("if 1 < 2:\n    say(\"yes\")\nelse:\n    say(\"no\")\n", false);
    let f = main_fn(&m);
    assert_eq!(f.blocks.len(), 1, "{}", m);
    assert_eq!(count_insts(f, |i| matches!(i, Inst::DataAddr { .. })), 1);
}

#[test]
fn dead_pure_code_is_eliminated_but_side_effects_stay() {
    use flashc::ir::*;
    let m = lowered("unused = 7 * 6\nsay(1)\n", false);
    let f = main_fn(&m);
    assert!(!f.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, Inst::IConst { val: 42, .. })), "{}", m);
    assert_eq!(count_insts(f, |i| matches!(i, Inst::Call { .. })), 2); // print + newline
}

#[test]
fn release_builds_inline_small_functions() {
    use flashc::ir::*;
    let src = "fn sq(x: Int) -> Int:\n    return x * x\ntotal = 0\nfor i in 0..10:\n    total += sq(i)\nsay(total)\n";
    let dev = lowered(src, false);
    let rel = lowered(src, true);
    let calls_user = |m: &Module| count_insts(main_fn(m), |i| matches!(i, Inst::Call { callee: Callee::Func(_), .. }));
    assert_eq!(calls_user(&dev), 1, "{}", dev);
    assert_eq!(calls_user(&rel), 0, "{}", rel);
    assert!(rel.funcs.len() < dev.funcs.len() || rel.funcs.len() == 1, "unused function should be removed");
}

#[test]
fn recursive_functions_are_not_inlined_forever() {
    let m = lowered("fn f(n: Int) -> Int:\n    if n < 2:\n        return n\n    return f(n - 1) + f(n - 2)\nsay(f(5))\n", true);
    assert!(m.funcs.iter().any(|f| f.name.starts_with("f_")), "recursive function must survive");
}

#[test]
fn drops_are_emitted_for_owned_locals_and_conditional_moves_use_flags() {
    use flashc::ir::*;
    // owned string dropped at scope end: a call to fl_str_drop exists
    let m = lowered("s = \"a\" + \"b\"\nsay(s)\n", false);
    let drop_idx = m.externs.iter().position(|e| e.name == "fl_str_drop").expect("fl_str_drop declared");
    assert!(count_insts(main_fn(&m), |i| matches!(i, Inst::Call { callee: Callee::Extern(e), .. } if *e as usize == drop_idx)) >= 1);
    // a moved-out string is not dropped by the owner on that path: needs a drop flag branch
    let m = lowered("fn eat(own s: String):\n    say(s)\ns = \"a\" + \"b\"\nif len(s) > 1:\n    eat(s)\n", false);
    assert!(main_fn(&m).blocks.len() > 3, "{}", m);
}

fn uses_bounds_check(src: &str) -> bool {
    lowered(src, false).externs.iter().any(|e| e.name == "fl_panic_bounds")
}

#[test]
fn bounds_checks_are_removed_only_when_provably_safe() {
    // provably in range: index is the loop variable of 0..len(xs) and xs is not modified
    assert!(!uses_bounds_check("xs = [1, 2, 3]\ntotal = 0\nfor i in 0..len(xs):\n    total += xs[i]\nsay(total)\n"));
    // an arbitrary index keeps the check
    assert!(uses_bounds_check("xs = [1, 2, 3]\ni = 2\nsay(xs[i])\n"));
    // the list shrinks inside the loop: keep the check
    assert!(uses_bounds_check("xs = [1, 2, 3]\nfor i in 0..len(xs):\n    say(xs[i])\n    pop(xs)\n"));
    // the loop variable is reassigned: keep the check
    assert!(uses_bounds_check("xs = [1, 2, 3]\nfor i in 0..len(xs):\n    i = i + 5\n    say(xs[i])\n"));
    // off-by-one: i + 1 may be out of range
    assert!(uses_bounds_check("xs = [1, 2, 3]\nfor i in 0..len(xs):\n    say(xs[i + 1])\n"));
    // inclusive range goes one past the end
    assert!(uses_bounds_check("xs = [1, 2, 3]\nfor i in 0..=len(xs):\n    say(xs[i])\n"));
    // a different list
    assert!(uses_bounds_check("xs = [1, 2, 3]\nys = [1]\nfor i in 0..len(xs):\n    say(ys[i])\n"));
}
