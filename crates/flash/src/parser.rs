//! Indentation-aware recursive-descent parser with error recovery.

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics};
use crate::lexer::{lex, lex_inline, FPart, Tok, Token};
use crate::source::Span;

type PResult<T> = Result<T, Diagnostic>;

pub fn parse_module(src: &str, file: u32, diags: &mut Diagnostics) -> Module {
    let tokens = lex(src, file, diags);
    let mut p = Parser { toks: tokens, pos: 0, file, diags, src: src.to_string() };
    p.module()
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    file: u32,
    diags: &'a mut Diagnostics,
    src: String,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos.min(self.toks.len() - 1)].span
    }
    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: Tok, what: &str) -> PResult<Token> {
        if self.at(&t) {
            Ok(self.bump())
        } else {
            Err(self.unexpected(what))
        }
    }
    fn unexpected(&self, expected: &str) -> Diagnostic {
        let found = self.peek().clone();
        let mut sp = self.span();
        if matches!(found, Tok::Newline | Tok::Eof | Tok::Dedent) && self.pos > 0 {
            // Point at the end of the previous token: that is where something is missing.
            let p = self.prev_span();
            sp = Span { file: p.file, lo: p.hi, hi: p.hi + 1 };
            if sp.hi as usize > self.src.len() {
                sp = p;
            }
        }
        let mut d = Diagnostic::error(sp, format!("Expected {}, but found {}.", expected, found.describe()));
        if found == Tok::Indent {
            d = d.help(
                "This line is indented more than the line before it. Remove the extra spaces, or start a block with ':' on the line above.",
            );
        }
        d
    }

    // ---- recovery ----
    /// Skip to the end of the current logical line (and over any indented block that follows).
    fn recover(&mut self) {
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Newline => {
                    self.bump();
                    break;
                }
                // Stay on a dedent so the enclosing block can end properly.
                Tok::Dedent => return,
                _ => {
                    self.bump();
                }
            }
        }
        if self.at(&Tok::Indent) {
            let mut depth = 0;
            loop {
                match self.peek() {
                    Tok::Indent => depth += 1,
                    Tok::Dedent => {
                        depth -= 1;
                        if depth == 0 {
                            self.bump();
                            return;
                        }
                    }
                    Tok::Eof => return,
                    _ => {}
                }
                self.bump();
            }
        }
    }

    // ---- items ----
    fn module(&mut self) -> Module {
        let mut items = vec![];
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Eof) {
                break;
            }
            if self.at(&Tok::Dedent) {
                self.bump();
                continue;
            }
            match self.item() {
                Ok(it) => items.push(it),
                Err(d) => {
                    self.diags.push(d);
                    self.recover();
                }
            }
        }
        Module { file: self.file, items }
    }

    fn item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Fn => self.fn_def().map(Item::Fn),
            Tok::Struct => self.struct_def().map(Item::Struct),
            Tok::Extern => self.extern_fn().map(Item::ExternFn),
            Tok::Import => self.import().map(Item::Import),
            Tok::Use => self.use_plugin().map(Item::UsePlugin),
            Tok::Indent => {
                let sp = self.span();
                // Skip the whole unexpected block.
                let d = Diagnostic::error(sp, "This line is indented, but there is no block above it to belong to.")
                    .help("Remove the leading spaces, or put it under an `if`, `while`, `for` or `fn` line that ends with ':'.");
                Err(d)
            }
            _ => self.stmt().map(Item::Stmt),
        }
    }

    fn ident(&mut self, what: &str) -> PResult<(String, Span)> {
        if let Tok::Ident(s) = self.peek().clone() {
            let sp = self.span();
            self.bump();
            Ok((s, sp))
        } else {
            Err(self.unexpected(what))
        }
    }

    fn import(&mut self) -> PResult<ImportDecl> {
        let start = self.bump().span;
        let (first, _) = self.ident("a module name after `import`")?;
        let mut path = vec![first];
        while self.eat(&Tok::Dot) {
            let (n, _) = self.ident("a module name")?;
            path.push(n);
        }
        let span = start.to(self.prev_span());
        self.end_of_stmt()?;
        Ok(ImportDecl { path, span })
    }

    fn use_plugin(&mut self) -> PResult<PluginDecl> {
        let start = self.bump().span;
        let (kind_name, ksp) = self.ident("`rust_plugin` or `cpp_plugin` after `use`")?;
        let kind = match kind_name.as_str() {
            "rust_plugin" => PluginKind::Rust,
            "cpp_plugin" => PluginKind::Cpp,
            _ => {
                return Err(Diagnostic::error(ksp, format!("Unknown kind of `use`: '{}'.", kind_name))
                    .help("Write `use rust_plugin \"name\"` or `use cpp_plugin \"name\"`."))
            }
        };
        let name = match self.peek().clone() {
            Tok::Str(s) => {
                self.bump();
                s
            }
            _ => return Err(self.unexpected("a plugin name in quotes")),
        };
        let span = start.to(self.prev_span());
        self.end_of_stmt()?;
        Ok(PluginDecl { kind, name, span })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let sp = self.span();
        // tuple type: (Int, String)
        if self.at(&Tok::LParen) {
            self.bump();
            let mut args = vec![];
            while !self.at(&Tok::RParen) {
                args.push(self.type_expr()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(Tok::RParen, "')' to close the tuple type")?;
            return Ok(TypeExpr { name: "Tuple".into(), args, span: sp.to(self.prev_span()) });
        }
        let name = match self.peek().clone() {
            Tok::Ident(s) => {
                self.bump();
                s
            }
            Tok::NoneLit => {
                self.bump();
                "None".to_string()
            }
            _ => return Err(self.unexpected("a type name (like Int, Float, String)")),
        };
        let mut args = vec![];
        if self.eat(&Tok::LBracket) {
            loop {
                args.push(self.type_expr()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(Tok::RBracket, "']' to close the type arguments")?;
        }
        Ok(TypeExpr { name, args, span: sp.to(self.prev_span()) })
    }

    fn fn_def(&mut self) -> PResult<FnDef> {
        let start = self.bump().span;
        let (mut name, mut name_span) = self.ident("a function name after `fn`")?;
        // `fn Player.level_up(...)` defines a method of the struct Player.
        let mut owner = None;
        if self.at(&Tok::Dot) {
            self.bump();
            let (method, msp) = self.ident("a method name after '.'")?;
            owner = Some(name.clone());
            name = format!("{}.{}", name, method);
            name_span = name_span.to(msp);
        }
        self.expect(Tok::LParen, "'(' after the function name")?;
        let mut params = vec![];
        while !self.at(&Tok::RParen) {
            let psp = self.span();
            let mode = if self.eat(&Tok::Mut) {
                ParamMode::Mut
            } else if self.eat(&Tok::Own) {
                ParamMode::Own
            } else {
                ParamMode::Borrow
            };
            let (pname, _) = self.ident("a parameter name")?;
            let mut ty = if self.eat(&Tok::Colon) { Some(self.type_expr()?) } else { None };
            if ty.is_none() && pname == "self" && params.is_empty() {
                if let Some(o) = &owner {
                    ty = Some(TypeExpr { name: o.clone(), args: vec![], span: psp });
                }
            }
            params.push(Param { name: pname, ty, mode, span: psp.to(self.prev_span()) });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "')' to close the parameter list")?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.type_expr()?) } else { None };
        let header = start.to(self.prev_span());
        let body = self.block("':' after the function header")?;
        Ok(FnDef { owner, name, name_span, params, ret, body, span: header })
    }

    /// `extern fn name(a: Int, b: String) -> Int`
    fn extern_fn(&mut self) -> PResult<ExternFnDef> {
        let start = self.bump().span;
        self.expect(Tok::Fn, "`fn` after `extern`")?;
        let (name, name_span) = self.ident("a function name after `extern fn`")?;
        self.expect(Tok::LParen, "'(' after the function name")?;
        let mut params = vec![];
        while !self.at(&Tok::RParen) {
            let (pname, _) = self.ident("a parameter name")?;
            self.expect(Tok::Colon, "':' and a type (extern functions need a type for every parameter)")?;
            let ty = self.type_expr()?;
            params.push((pname, ty));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "')' to close the parameter list")?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.type_expr()?) } else { None };
        let span = start.to(self.prev_span());
        self.end_of_stmt()?;
        Ok(ExternFnDef { name, name_span, params, ret, span })
    }

    fn struct_def(&mut self) -> PResult<StructDef> {
        let start = self.bump().span;
        let (name, name_span) = self.ident("a struct name after `struct`")?;
        let header = start.to(self.prev_span());
        self.expect(Tok::Colon, "':' after the struct name")?;
        self.expect(Tok::Newline, "a new line after ':'")?;
        self.expect(Tok::Indent, "an indented list of fields")?;
        let mut fields = vec![];
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Dedent) || self.at(&Tok::Eof) {
                break;
            }
            if self.eat(&Tok::Pass) {
                continue;
            }
            let r: PResult<()> = (|| {
                let (fname, fsp) = self.ident("a field name")?;
                self.expect(Tok::Colon, "':' and a type after the field name")?;
                let ty = self.type_expr()?;
                fields.push(FieldDef { name: fname, ty, span: fsp.to(self.prev_span()) });
                self.end_of_stmt()
            })();
            if let Err(d) = r {
                self.diags.push(d);
                self.recover();
            }
        }
        self.eat(&Tok::Dedent);
        Ok(StructDef { name, name_span, fields, span: header })
    }

    // ---- statements ----
    fn end_of_stmt(&mut self) -> PResult<()> {
        match self.peek() {
            Tok::Newline => {
                self.bump();
                Ok(())
            }
            Tok::Eof | Tok::Dedent => Ok(()),
            _ => Err(self.unexpected("the end of the line")),
        }
    }

    /// `:` NEWLINE INDENT stmts DEDENT   or   `:` simple-stmt
    fn block(&mut self, colon_what: &str) -> PResult<Vec<Stmt>> {
        self.expect(Tok::Colon, colon_what)?;
        if !self.at(&Tok::Newline) {
            // single-line body
            let s = self.stmt()?;
            return Ok(vec![s]);
        }
        self.bump();
        if !self.at(&Tok::Indent) {
            let sp = self.span();
            return Err(Diagnostic::error(sp, "Expected an indented block after ':'.")
                .help("Indent the lines that belong to this block (4 spaces is the convention), or write `pass` for an empty block."));
        }
        self.bump();
        let mut stmts = vec![];
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Dedent) {
                self.bump();
                break;
            }
            if self.at(&Tok::Eof) {
                break;
            }
            match self.stmt() {
                Ok(s) => stmts.push(s),
                Err(d) => {
                    self.diags.push(d);
                    self.recover();
                }
            }
        }
        Ok(stmts)
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        match self.peek() {
            Tok::If => self.if_stmt(),
            Tok::While => {
                self.bump();
                let cond = self.expr()?;
                let body = self.block("':' after the `while` condition")?;
                Ok(Stmt { kind: StmtKind::While { cond, body }, span: start })
            }
            Tok::For => self.for_stmt(),
            Tok::Fn => Err(Diagnostic::error(start, "Functions can only be defined at the top level of a file.")
                .help("Move this `fn` out of the indented block.")),
            Tok::Struct => Err(Diagnostic::error(start, "Structs can only be defined at the top level of a file.")),
            Tok::Break => {
                self.bump();
                self.end_of_stmt()?;
                Ok(Stmt { kind: StmtKind::Break, span: start })
            }
            Tok::Continue => {
                self.bump();
                self.end_of_stmt()?;
                Ok(Stmt { kind: StmtKind::Continue, span: start })
            }
            Tok::Pass => {
                self.bump();
                self.end_of_stmt()?;
                Ok(Stmt { kind: StmtKind::Pass, span: start })
            }
            Tok::Return => {
                self.bump();
                let v = if matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent) { None } else { Some(self.expr_or_bare_tuple()?) };
                let span = start.to(self.prev_span());
                self.end_of_stmt()?;
                Ok(Stmt { kind: StmtKind::Return(v), span })
            }
            _ => self.simple_stmt(),
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        self.bump();
        let mut branches = vec![];
        let cond = self.expr()?;
        let body = self.block("':' after the `if` condition")?;
        branches.push((cond, body));
        let mut else_body = None;
        loop {
            // skip blank NEWLINEs between branches (none are emitted, but be safe)
            if self.at(&Tok::Elif) {
                self.bump();
                let c = self.expr()?;
                let b = self.block("':' after the `elif` condition")?;
                branches.push((c, b));
            } else if self.at(&Tok::Else) {
                self.bump();
                if self.at(&Tok::If) {
                    let sp = self.span();
                    return Err(Diagnostic::error(sp, "Flash uses `elif`, not `else if`.").help("Write `elif condition:` instead."));
                }
                else_body = Some(self.block("':' after `else`")?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt { kind: StmtKind::If { branches, else_body }, span: start })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        self.bump();
        let (var, var_span) = self.ident("a loop variable name after `for`")?;
        self.expect(Tok::In, "`in` after the loop variable")?;
        let first = self.expr()?;
        let iter = if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
            let inclusive = self.at(&Tok::DotDotEq);
            self.bump();
            let hi = self.expr()?;
            ForIter::Range { lo: first, hi, inclusive }
        } else {
            ForIter::Expr(first)
        };
        let body = self.block("':' after the `for` header")?;
        Ok(Stmt { kind: StmtKind::For { var, var_span, iter, body }, span: start })
    }

    fn simple_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let e = self.expr()?;
        // `a, b = value`
        if self.at(&Tok::Comma) {
            let mut targets = vec![e];
            while self.eat(&Tok::Comma) {
                targets.push(self.expr()?);
            }
            if !self.at(&Tok::Assign) {
                return Err(self.unexpected("'=' after the names to unpack"));
            }
            self.bump();
            let value = self.expr_or_bare_tuple()?;
            let span = start.to(self.prev_span());
            self.end_of_stmt()?;
            return Ok(Stmt { kind: StmtKind::Unpack { targets, value }, span });
        }
        let kind = match self.peek() {
            Tok::Assign => {
                self.bump();
                let value = self.expr_or_bare_tuple()?;
                StmtKind::Assign { target: e, ty: None, value }
            }
            Tok::Colon => {
                self.bump();
                let ty = self.type_expr()?;
                self.expect(Tok::Assign, "'=' and a value after the type")?;
                let value = self.expr()?;
                StmtKind::Assign { target: e, ty: Some(ty), value }
            }
            Tok::PlusEq | Tok::MinusEq | Tok::StarEq | Tok::SlashEq | Tok::PercentEq => {
                let op = match self.bump().tok {
                    Tok::PlusEq => BinOp::Add,
                    Tok::MinusEq => BinOp::Sub,
                    Tok::StarEq => BinOp::Mul,
                    Tok::SlashEq => BinOp::Div,
                    _ => BinOp::Rem,
                };
                let value = self.expr()?;
                StmtKind::AugAssign { target: e, op, value }
            }
            _ => StmtKind::Expr(e),
        };
        let span = start.to(self.prev_span());
        if matches!(self.peek(), Tok::Assign) {
            return Err(Diagnostic::error(self.span(), "A statement can only assign once.").help("Write each assignment on its own line."));
        }
        self.end_of_stmt()?;
        Ok(Stmt { kind, span })
    }

    // ---- expressions ----
    /// An expression, or several separated by commas (a tuple without parentheses): `return a, b`.
    fn expr_or_bare_tuple(&mut self) -> PResult<Expr> {
        let first = self.expr()?;
        if !self.at(&Tok::Comma) {
            return Ok(first);
        }
        let lo = first.span;
        let mut elems = vec![first];
        while self.eat(&Tok::Comma) {
            if matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent) {
                break;
            }
            elems.push(self.expr()?);
        }
        Ok(Expr { kind: ExprKind::Tuple(elems), span: lo.to(self.prev_span()) })
    }

    pub fn expr(&mut self) -> PResult<Expr> {
        self.or_expr()
    }

    fn bin(&self, op: BinOp, l: Expr, r: Expr) -> Expr {
        let span = l.span.to(r.span);
        Expr { kind: ExprKind::Binary(op, Box::new(l), Box::new(r)), span }
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut l = self.and_expr()?;
        while self.eat(&Tok::Or) {
            let r = self.and_expr()?;
            l = self.bin(BinOp::Or, l, r);
        }
        Ok(l)
    }
    fn and_expr(&mut self) -> PResult<Expr> {
        let mut l = self.not_expr()?;
        while self.eat(&Tok::And) {
            let r = self.not_expr()?;
            l = self.bin(BinOp::And, l, r);
        }
        Ok(l)
    }
    fn not_expr(&mut self) -> PResult<Expr> {
        if self.at(&Tok::Not) {
            let sp = self.bump().span;
            let e = self.not_expr()?;
            let span = sp.to(e.span);
            return Ok(Expr { kind: ExprKind::Unary(UnOp::Not, Box::new(e)), span });
        }
        self.cmp_expr()
    }
    fn cmp_expr(&mut self) -> PResult<Expr> {
        let l = self.bitor_expr()?;
        let op = match self.peek() {
            Tok::EqEq => BinOp::Eq,
            Tok::NotEq => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(l),
        };
        self.bump();
        let r = self.bitor_expr()?;
        if matches!(self.peek(), Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge) {
            return Err(
                Diagnostic::error(self.span(), "Comparisons cannot be chained.").help("Write `a < b and b < c` instead of `a < b < c`.")
            );
        }
        Ok(self.bin(op, l, r))
    }
    fn bitor_expr(&mut self) -> PResult<Expr> {
        let mut l = self.bitxor_expr()?;
        while self.eat(&Tok::Pipe) {
            let r = self.bitxor_expr()?;
            l = self.bin(BinOp::BitOr, l, r);
        }
        Ok(l)
    }
    fn bitxor_expr(&mut self) -> PResult<Expr> {
        let mut l = self.bitand_expr()?;
        while self.eat(&Tok::Caret) {
            let r = self.bitand_expr()?;
            l = self.bin(BinOp::BitXor, l, r);
        }
        Ok(l)
    }
    fn bitand_expr(&mut self) -> PResult<Expr> {
        let mut l = self.shift_expr()?;
        while self.eat(&Tok::Amp) {
            let r = self.shift_expr()?;
            l = self.bin(BinOp::BitAnd, l, r);
        }
        Ok(l)
    }
    fn shift_expr(&mut self) -> PResult<Expr> {
        let mut l = self.add_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                _ => break,
            };
            self.bump();
            let r = self.add_expr()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }
    fn add_expr(&mut self) -> PResult<Expr> {
        let mut l = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let r = self.mul_expr()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }
    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut l = self.unary_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let r = self.unary_expr()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }
    fn unary_expr(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Tilde => UnOp::BitNot,
            _ => return self.postfix_expr(),
        };
        let sp = self.bump().span;
        // Fold `-literal` (a minus directly in front of a number token) so that the minimum value
        // `-9223372036854775808` is representable; `-(-x)` and `-(literal)` stay real operations.
        let direct_literal = op == UnOp::Neg && matches!(self.peek(), Tok::Int(_) | Tok::Float(_));
        let e = self.unary_expr()?;
        let span = sp.to(e.span);
        if direct_literal {
            match e.kind {
                ExprKind::Int(v) => return Ok(Expr { kind: ExprKind::Int(-v), span }),
                ExprKind::Float(v) => return Ok(Expr { kind: ExprKind::Float(-v), span }),
                _ => {}
            }
        }
        Ok(Expr { kind: ExprKind::Unary(op, Box::new(e)), span })
    }

    fn postfix_expr(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.bump();
                    let args = self.call_args()?;
                    let end = self.expect(Tok::RParen, "')' to close the call")?.span;
                    let span = e.span.to(end);
                    e = Expr { kind: ExprKind::Call { callee: Box::new(e), args }, span };
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.expr()?;
                    let end = self.expect(Tok::RBracket, "']' to close the index")?.span;
                    let span = e.span.to(end);
                    e = Expr { kind: ExprKind::Index { base: Box::new(e), index: Box::new(idx) }, span };
                }
                Tok::Dot => {
                    self.bump();
                    // tuple element: t.0
                    if let Tok::Int(n) = self.peek().clone() {
                        let nsp = self.bump().span;
                        let span = e.span.to(nsp);
                        e = Expr { kind: ExprKind::Field { base: Box::new(e), name: n.to_string(), name_span: nsp }, span };
                        continue;
                    }
                    if let Tok::Float(_) = self.peek() {
                        return Err(Diagnostic::error(self.span(), "Write nested tuple elements with parentheses.")
                            .help("For example `(t.0).1` instead of `t.0.1`."));
                    }
                    let (name, nsp) = self.ident("a field or method name after '.'")?;
                    let span = e.span.to(nsp);
                    e = Expr { kind: ExprKind::Field { base: Box::new(e), name, name_span: nsp }, span };
                }
                Tok::Question => {
                    let q = self.bump().span;
                    let span = e.span.to(q);
                    e = Expr { kind: ExprKind::Try(Box::new(e)), span };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn call_args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = vec![];
        while !self.at(&Tok::RParen) {
            let mut name = None;
            if let (Tok::Ident(n), Tok::Assign) = (self.peek().clone(), self.peek_at(1).clone()) {
                self.bump();
                self.bump();
                name = Some(n);
            }
            let value = self.expr()?;
            args.push(Arg { name, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(args)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let sp = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::FStr(parts) => {
                self.bump();
                return self.fstring(parts, sp);
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::NoneLit => {
                self.bump();
                ExprKind::None
            }
            Tok::Ident(n) => {
                self.bump();
                ExprKind::Name(n)
            }
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                if self.at(&Tok::Comma) {
                    // a tuple: (a, b) or (a,)
                    let mut elems = vec![e];
                    while self.eat(&Tok::Comma) {
                        if self.at(&Tok::RParen) {
                            break;
                        }
                        elems.push(self.expr()?);
                    }
                    let end = self.expect(Tok::RParen, "')' to close the tuple")?.span;
                    return Ok(Expr { kind: ExprKind::Tuple(elems), span: sp.to(end) });
                }
                self.expect(Tok::RParen, "')' to close the parenthesis")?;
                return Ok(e);
            }
            Tok::LBracket => {
                self.bump();
                let mut elems = vec![];
                while !self.at(&Tok::RBracket) {
                    elems.push(self.expr()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let end = self.expect(Tok::RBracket, "']' to close the list")?.span;
                return Ok(Expr { kind: ExprKind::List(elems), span: sp.to(end) });
            }
            _ => return Err(self.unexpected("a value or expression")),
        };
        Ok(Expr { kind, span: sp })
    }

    /// Desugar `f"a{x}b"` into `"a" + str(x) + "b"`.
    fn fstring(&mut self, parts: Vec<FPart>, sp: Span) -> PResult<Expr> {
        let mut acc: Option<Expr> = None;
        let mut only_expr = parts.len() == 1;
        for part in parts {
            let piece = match part {
                FPart::Lit(s) => {
                    only_expr = false;
                    Expr { kind: ExprKind::Str(s), span: sp }
                }
                FPart::Expr(text, off) => {
                    let toks = lex_inline(&text, self.file, off, self.diags);
                    let mut sub = Parser { toks, pos: 0, file: self.file, diags: self.diags, src: text.clone() };
                    let e = sub.expr()?;
                    if !matches!(sub.peek(), Tok::Eof) {
                        return Err(sub.unexpected("the end of the expression inside { }"));
                    }
                    let espan = e.span;
                    Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(Expr { kind: ExprKind::Name("str".into()), span: espan }),
                            args: vec![Arg { name: None, value: e }],
                        },
                        span: espan,
                    }
                }
            };
            acc = Some(match acc {
                None => piece,
                Some(a) => Expr { kind: ExprKind::Binary(BinOp::Add, Box::new(a), Box::new(piece)), span: sp },
            });
        }
        let _ = only_expr;
        Ok(acc.unwrap_or(Expr { kind: ExprKind::Str(String::new()), span: sp }))
    }
}
