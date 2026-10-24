//! Lexer with Python-style indentation tokens.
//!
//! Indentation rules (see LANGUAGE_SPEC.md):
//! * Only spaces may be used to indent; a tab in indentation is an error.
//! * Any consistent width is accepted; a dedent must return to an enclosing level.
//! * Blank lines and comment-only lines are ignored.
//! * Newlines inside (), [] are ignored (implicit line joining).

use crate::diag::{Diagnostic, Diagnostics};
use crate::source::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum FPart {
    Lit(String),
    /// Source text of the embedded expression and the byte offset where it starts.
    Expr(String, usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i128),
    Float(f64),
    Str(String),
    FStr(Vec<FPart>),
    // keywords
    Fn,
    Struct,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Pass,
    And,
    Or,
    Not,
    True,
    False,
    NoneLit,
    Use,
    Import,
    Mut,
    Own,
    Extern,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,
    Question,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("the name '{}'", s),
            Tok::Int(_) | Tok::Float(_) => "a number".into(),
            Tok::Str(_) | Tok::FStr(_) => "a string".into(),
            Tok::Newline => "the end of the line".into(),
            Tok::Indent => "extra indentation".into(),
            Tok::Dedent => "the end of an indented block".into(),
            Tok::Eof => "the end of the file".into(),
            other => format!("'{}'", other.text()),
        }
    }
    pub fn text(&self) -> &'static str {
        match self {
            Tok::Fn => "fn",
            Tok::Struct => "struct",
            Tok::If => "if",
            Tok::Elif => "elif",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::For => "for",
            Tok::In => "in",
            Tok::Return => "return",
            Tok::Break => "break",
            Tok::Continue => "continue",
            Tok::Pass => "pass",
            Tok::And => "and",
            Tok::Or => "or",
            Tok::Not => "not",
            Tok::True => "True",
            Tok::False => "False",
            Tok::NoneLit => "None",
            Tok::Use => "use",
            Tok::Import => "import",
            Tok::Mut => "mut",
            Tok::Own => "own",
            Tok::Extern => "extern",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Colon => ":",
            Tok::Dot => ".",
            Tok::DotDot => "..",
            Tok::DotDotEq => "..=",
            Tok::Arrow => "->",
            Tok::Question => "?",
            Tok::Assign => "=",
            Tok::PlusEq => "+=",
            Tok::MinusEq => "-=",
            Tok::StarEq => "*=",
            Tok::SlashEq => "/=",
            Tok::PercentEq => "%=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Caret => "^",
            Tok::Tilde => "~",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            _ => "?",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: u32,
    base: usize,
    /// When lexing an embedded f-string expression there is no indentation.
    inline: bool,
    indents: Vec<usize>,
    depth: i32,
    out: Vec<Token>,
    diags: &'a mut Diagnostics,
}

pub fn lex(src: &str, file: u32, diags: &mut Diagnostics) -> Vec<Token> {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        file,
        base: 0,
        inline: false,
        indents: vec![0],
        depth: 0,
        out: vec![],
        diags,
    };
    lx.run();
    lx.out
}

/// Lex a snippet embedded in a larger file (used for f-string expressions).
pub fn lex_inline(src: &str, file: u32, base: usize, diags: &mut Diagnostics) -> Vec<Token> {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        file,
        base,
        inline: true,
        indents: vec![0],
        depth: 1,
        out: vec![],
        diags,
    };
    lx.run();
    lx.out
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "fn" => Tok::Fn,
        "struct" => Tok::Struct,
        "if" => Tok::If,
        "elif" => Tok::Elif,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "in" => Tok::In,
        "return" => Tok::Return,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "pass" => Tok::Pass,
        "and" => Tok::And,
        "or" => Tok::Or,
        "not" => Tok::Not,
        "True" => Tok::True,
        "False" => Tok::False,
        "None" => Tok::NoneLit,
        "use" => Tok::Use,
        "import" => Tok::Import,
        "mut" => Tok::Mut,
        "own" => Tok::Own,
        "extern" => Tok::Extern,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    fn span(&self, lo: usize, hi: usize) -> Span {
        Span::new(self.file, self.base + lo, self.base + hi)
    }

    fn err(&mut self, lo: usize, hi: usize, msg: impl Into<String>) {
        let sp = self.span(lo, hi.max(lo + 1));
        self.diags.push(Diagnostic::error(sp, msg));
    }

    fn push(&mut self, tok: Tok, lo: usize, hi: usize) {
        let span = self.span(lo, hi);
        self.out.push(Token { tok, span });
    }

    fn peek(&self, off: usize) -> u8 {
        *self.bytes.get(self.pos + off).unwrap_or(&0)
    }

    fn run(&mut self) {
        let mut at_line_start = !self.inline;
        loop {
            if at_line_start {
                at_line_start = false;
                if !self.handle_indentation() {
                    break;
                }
                continue;
            }
            if self.pos >= self.bytes.len() {
                break;
            }
            let c = self.peek(0);
            match c {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'#' => {
                    while self.pos < self.bytes.len() && self.peek(0) != b'\n' {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    let p = self.pos;
                    self.pos += 1;
                    if self.depth == 0 && !self.inline {
                        // Emit NEWLINE only if the logical line had tokens.
                        if let Some(last) = self.out.last() {
                            if !matches!(last.tok, Tok::Newline | Tok::Indent | Tok::Dedent) {
                                self.push(Tok::Newline, p, p + 1);
                            }
                        }
                        at_line_start = true;
                    }
                }
                b'\\' if self.peek(1) == b'\n' => self.pos += 2,
                b'"' | b'\'' => self.string(false),
                b'f' if self.peek(1) == b'"' || self.peek(1) == b'\'' => {
                    self.pos += 1;
                    self.string(true)
                }
                c if c.is_ascii_digit() => self.number(),
                c if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 => self.ident(),
                _ => self.punct(),
            }
        }
        let end = self.src.len();
        if !self.inline {
            if let Some(last) = self.out.last() {
                if !matches!(last.tok, Tok::Newline | Tok::Dedent | Tok::Indent) {
                    self.push(Tok::Newline, end, end);
                }
            }
            while self.indents.len() > 1 {
                self.indents.pop();
                self.push(Tok::Dedent, end, end);
            }
        }
        self.push(Tok::Eof, end, end);
    }

    /// Process leading whitespace of a new line. Returns false at EOF.
    fn handle_indentation(&mut self) -> bool {
        loop {
            let line_start = self.pos;
            let mut width = 0usize;
            let mut saw_tab = false;
            while self.pos < self.bytes.len() {
                match self.peek(0) {
                    b' ' => {
                        width += 1;
                        self.pos += 1;
                    }
                    b'\t' => {
                        // Reported below; counted as 4 columns so the rest of the file still parses sensibly.
                        saw_tab = true;
                        width += 4;
                        self.pos += 1;
                    }
                    b'\r' => self.pos += 1,
                    _ => break,
                }
            }
            if self.pos >= self.bytes.len() {
                return false;
            }
            match self.peek(0) {
                b'\n' => {
                    self.pos += 1;
                    continue;
                }
                b'#' => {
                    while self.pos < self.bytes.len() && self.peek(0) != b'\n' {
                        self.pos += 1;
                    }
                    continue;
                }
                _ => {}
            }
            if saw_tab {
                self.err(line_start, self.pos, "Tabs are not allowed for indentation.");
                self.diags.items.last_mut().map(|d| {
                    d.help = Some("Use spaces (4 per level is the convention). Configure your editor to insert spaces for the Tab key.".into())
                });
            }
            let cur = *self.indents.last().unwrap();
            if width > cur {
                self.indents.push(width);
                self.push(Tok::Indent, line_start, self.pos);
            } else if width < cur {
                while *self.indents.last().unwrap() > width {
                    self.indents.pop();
                    self.push(Tok::Dedent, line_start, self.pos);
                }
                if *self.indents.last().unwrap() != width {
                    self.err(
                        line_start,
                        self.pos,
                        "This line is indented differently from any enclosing block.",
                    );
                    self.diags.items.last_mut().map(|d| {
                        d.help = Some("Dedent to the same column as an earlier line of the surrounding block.".into())
                    });
                    // Recover by treating it as the enclosing level.
                    self.indents.push(width);
                }
            }
            return true;
        }
    }

    fn ident(&mut self) {
        let lo = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.peek(0);
            if c == b'_' || c.is_ascii_alphanumeric() || c >= 0x80 {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = &self.src[lo..self.pos];
        let tok = keyword(s).unwrap_or_else(|| Tok::Ident(s.to_string()));
        self.push(tok, lo, self.pos);
    }

    fn number(&mut self) {
        let lo = self.pos;
        // hex / binary / octal
        if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X' | b'b' | b'B' | b'o' | b'O') {
            let radix = match self.peek(1) {
                b'x' | b'X' => 16,
                b'b' | b'B' => 2,
                _ => 8,
            };
            self.pos += 2;
            let ds = self.pos;
            while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                self.pos += 1;
            }
            let digits: String = self.src[ds..self.pos].chars().filter(|c| *c != '_').collect();
            match i128::from_str_radix(&digits, radix) {
                Ok(v) => self.push(Tok::Int(v), lo, self.pos),
                Err(_) => {
                    self.err(lo, self.pos, "This is not a valid number.");
                    self.push(Tok::Int(0), lo, self.pos);
                }
            }
            return;
        }
        while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
            self.pos += 1;
        }
        let mut is_float = false;
        // fraction: '.' followed by digit (so `0..10` stays a range)
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), b'e' | b'E')
            && (self.peek(1).is_ascii_digit()
                || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
        {
            is_float = true;
            self.pos += 2;
            while self.peek(0).is_ascii_digit() {
                self.pos += 1;
            }
        }
        let text: String = self.src[lo..self.pos].chars().filter(|c| *c != '_').collect();
        if is_float {
            match text.parse::<f64>() {
                Ok(v) => self.push(Tok::Float(v), lo, self.pos),
                Err(_) => {
                    self.err(lo, self.pos, "This is not a valid number.");
                    self.push(Tok::Float(0.0), lo, self.pos)
                }
            }
        } else {
            match text.parse::<i128>() {
                Ok(v) => self.push(Tok::Int(v), lo, self.pos),
                Err(_) => {
                    self.err(lo, self.pos, "This number is too large.");
                    self.push(Tok::Int(0), lo, self.pos)
                }
            }
        }
    }

    fn string(&mut self, fstr: bool) {
        let lo = if fstr { self.pos - 1 } else { self.pos };
        let quote = self.peek(0);
        self.pos += 1;
        let mut parts: Vec<FPart> = vec![];
        let mut cur = String::new();
        let mut closed = false;
        while self.pos < self.bytes.len() {
            let c = self.peek(0);
            if c == quote {
                self.pos += 1;
                closed = true;
                break;
            }
            if c == b'\n' {
                break;
            }
            if c == b'\\' {
                let e = self.peek(1);
                self.pos += 2;
                match e {
                    b'n' => cur.push('\n'),
                    b't' => cur.push('\t'),
                    b'r' => cur.push('\r'),
                    b'0' => cur.push('\0'),
                    b'\\' => cur.push('\\'),
                    b'"' => cur.push('"'),
                    b'\'' => cur.push('\''),
                    b'{' => cur.push('{'),
                    b'}' => cur.push('}'),
                    _ => {
                        self.err(self.pos - 2, self.pos, format!("Unknown escape sequence '\\{}'.", e as char));
                    }
                }
                continue;
            }
            if fstr && c == b'{' {
                if self.peek(1) == b'{' {
                    cur.push('{');
                    self.pos += 2;
                    continue;
                }
                // find matching '}'
                let start = self.pos + 1;
                let mut depth = 1;
                let mut p = start;
                while p < self.bytes.len() && self.bytes[p] != b'\n' {
                    match self.bytes[p] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    p += 1;
                }
                if p >= self.bytes.len() || self.bytes[p] != b'}' {
                    self.err(self.pos, self.pos + 1, "This '{' in the f-string is never closed with '}'.");
                    self.pos = p;
                    break;
                }
                if !cur.is_empty() {
                    parts.push(FPart::Lit(std::mem::take(&mut cur)));
                }
                let text = self.src[start..p].to_string();
                if text.trim().is_empty() {
                    self.err(start - 1, p + 1, "There is no expression between '{' and '}'.");
                }
                parts.push(FPart::Expr(text, self.base + start));
                self.pos = p + 1;
                continue;
            }
            if fstr && c == b'}' {
                if self.peek(1) == b'}' {
                    cur.push('}');
                    self.pos += 2;
                    continue;
                }
                self.err(self.pos, self.pos + 1, "A single '}' in an f-string must be written as '}}'.");
                self.pos += 1;
                continue;
            }
            // regular (possibly multi-byte) char
            let ch = self.src[self.pos..].chars().next().unwrap();
            cur.push(ch);
            self.pos += ch.len_utf8();
        }
        if !closed {
            self.err(lo, self.pos, "This string is missing its closing quote.");
        }
        if fstr {
            if !cur.is_empty() || parts.is_empty() {
                parts.push(FPart::Lit(cur));
            }
            self.push(Tok::FStr(parts), lo, self.pos);
        } else {
            self.push(Tok::Str(cur), lo, self.pos);
        }
    }

    fn punct(&mut self) {
        let lo = self.pos;
        let c = self.peek(0);
        let n = self.peek(1);
        let n2 = self.peek(2);
        let (tok, len) = match (c, n, n2) {
            (b'.', b'.', b'=') => (Tok::DotDotEq, 3),
            (b'.', b'.', _) => (Tok::DotDot, 2),
            (b'-', b'>', _) => (Tok::Arrow, 2),
            (b'=', b'=', _) => (Tok::EqEq, 2),
            (b'!', b'=', _) => (Tok::NotEq, 2),
            (b'<', b'=', _) => (Tok::Le, 2),
            (b'>', b'=', _) => (Tok::Ge, 2),
            (b'<', b'<', _) => (Tok::Shl, 2),
            (b'>', b'>', _) => (Tok::Shr, 2),
            (b'+', b'=', _) => (Tok::PlusEq, 2),
            (b'-', b'=', _) => (Tok::MinusEq, 2),
            (b'*', b'=', _) => (Tok::StarEq, 2),
            (b'/', b'=', _) => (Tok::SlashEq, 2),
            (b'%', b'=', _) => (Tok::PercentEq, 2),
            (b'(', _, _) => {
                self.depth += 1;
                (Tok::LParen, 1)
            }
            (b')', _, _) => {
                self.depth = (self.depth - 1).max(0);
                (Tok::RParen, 1)
            }
            (b'[', _, _) => {
                self.depth += 1;
                (Tok::LBracket, 1)
            }
            (b']', _, _) => {
                self.depth = (self.depth - 1).max(0);
                (Tok::RBracket, 1)
            }
            (b',', _, _) => (Tok::Comma, 1),
            (b':', _, _) => (Tok::Colon, 1),
            (b'.', _, _) => (Tok::Dot, 1),
            (b'?', _, _) => (Tok::Question, 1),
            (b'=', _, _) => (Tok::Assign, 1),
            (b'+', _, _) => (Tok::Plus, 1),
            (b'-', _, _) => (Tok::Minus, 1),
            (b'*', _, _) => (Tok::Star, 1),
            (b'/', _, _) => (Tok::Slash, 1),
            (b'%', _, _) => (Tok::Percent, 1),
            (b'&', _, _) => (Tok::Amp, 1),
            (b'|', _, _) => (Tok::Pipe, 1),
            (b'^', _, _) => (Tok::Caret, 1),
            (b'~', _, _) => (Tok::Tilde, 1),
            (b'<', _, _) => (Tok::Lt, 1),
            (b'>', _, _) => (Tok::Gt, 1),
            _ => {
                let ch = self.src[self.pos..].chars().next().unwrap();
                self.err(lo, lo + ch.len_utf8(), format!("I don't understand the character '{}'.", ch));
                self.pos += ch.len_utf8();
                return;
            }
        };
        self.pos += len;
        self.push(tok, lo, self.pos);
    }
}
