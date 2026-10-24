//! `flash fmt`: a token-based source formatter.
//!
//! * indentation is normalised to 4 spaces per level
//! * spacing around operators, commas and colons is normalised
//! * comments and string/number literals are preserved exactly
//! * runs of blank lines are collapsed (max 2 at top level, 1 inside blocks)
//! * trailing whitespace is removed and the file ends with one newline
//!
//! The formatter never changes the token stream, so formatted code means the same as the input
//! (the test-suite checks this by compiling and running before/after).

use crate::diag::Diagnostics;
use crate::lexer::{lex, Tok, Token};

/// Format `src`. Returns `Err` with a message if the file does not lex cleanly.
pub fn format_source(src: &str) -> Result<String, String> {
    let mut diags = Diagnostics::default();
    let toks = lex(src, 0, &mut diags);
    if diags.has_errors() {
        return Err("the file has syntax errors; run `flash check` first".into());
    }
    // physical line table
    let lines: Vec<&str> = src.split('\n').collect();
    let line_of = |pos: usize| -> usize { src[..pos.min(src.len())].bytes().filter(|b| *b == b'\n').count() };

    // Split the token stream into logical lines with their depth.
    struct Logical {
        toks: Vec<Token>,
        depth: usize,
        first_line: usize,
        last_line: usize,
        src_col: usize,
    }
    let mut logicals: Vec<Logical> = vec![];
    let mut depth = 0usize;
    let mut cur: Vec<Token> = vec![];
    let mut cur_depth = 0usize;
    for t in &toks {
        match t.tok {
            Tok::Indent => depth += 1,
            Tok::Dedent => depth = depth.saturating_sub(1),
            Tok::Newline | Tok::Eof => {
                if !cur.is_empty() {
                    let first_line = line_of(cur[0].span.lo as usize);
                    let last_line = line_of(cur.last().unwrap().span.hi.saturating_sub(1) as usize);
                    let first_pos = cur[0].span.lo as usize;
                    let src_col = first_pos - src[..first_pos].rfind('\n').map(|p| p + 1).unwrap_or(0);
                    logicals.push(Logical { toks: std::mem::take(&mut cur), depth: cur_depth, first_line, last_line, src_col });
                }
            }
            _ => {
                if cur.is_empty() {
                    cur_depth = depth;
                }
                cur.push(t.clone());
            }
        }
    }

    // Which physical lines belong to a logical line?
    let mut owner: Vec<Option<usize>> = vec![None; lines.len()];
    for (i, l) in logicals.iter().enumerate() {
        for ln in l.first_line..=l.last_line.min(lines.len() - 1) {
            owner[ln] = Some(i);
        }
    }

    let mut out = String::new();
    let mut blank_run = 0usize;
    let mut prev_depth = 0usize;
    let next_logical_depth = |from: usize| -> Option<usize> { (from..lines.len()).find_map(|ln| owner[ln].map(|i| logicals[i].depth)) };
    let mut emitted_any = false;
    let mut ln = 0;
    while ln < lines.len() {
        let text = lines[ln];
        match owner[ln] {
            Some(i) => {
                let l = &logicals[i];
                if ln != l.first_line {
                    ln += 1;
                    continue;
                }
                if emitted_any && blank_run > 0 {
                    let max = if l.depth == 0 { 2 } else { 1 };
                    for _ in 0..blank_run.min(max) {
                        out.push('\n');
                    }
                }
                blank_run = 0;
                let body = render_line(src, &l.toks);
                out.push_str(&"    ".repeat(l.depth));
                out.push_str(&body);
                // trailing comment: after the last token on the last physical line
                let last_tok_end = l.toks.last().unwrap().span.hi as usize;
                let rest_start = last_tok_end;
                let line_end = src[rest_start..].find('\n').map(|p| rest_start + p).unwrap_or(src.len());
                let rest = &src[rest_start..line_end];
                if let Some(ci) = rest.find('#') {
                    out.push_str("  ");
                    out.push_str(rest[ci..].trim_end());
                }
                out.push('\n');
                emitted_any = true;
                prev_depth = l.depth;
                ln = l.last_line + 1;
                continue;
            }
            None => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    blank_run += 1;
                } else if trimmed.starts_with('#') {
                    if emitted_any && blank_run > 0 {
                        for _ in 0..blank_run.min(2) {
                            out.push('\n');
                        }
                    }
                    blank_run = 0;
                    // Comment indentation: align with the nearest enclosing block whose source
                    // column is not to the right of the comment.
                    let col = text.len() - text.trim_start().len();
                    let next = next_logical_depth(ln + 1).unwrap_or(0);
                    let prev_idx = (0..ln).rev().find_map(|l| owner[l]);
                    let mut d = next;
                    if let Some(pi) = prev_idx {
                        if next < prev_depth {
                            for cand in (next..=prev_depth).rev() {
                                let found = (0..=pi).rev().find(|j| logicals[*j].depth == cand);
                                if let Some(j) = found {
                                    if logicals[j].src_col <= col {
                                        d = cand;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    out.push_str(&"    ".repeat(d));
                    out.push_str(trimmed);
                    out.push('\n');
                    emitted_any = true;
                }
            }
        }
        ln += 1;
    }
    Ok(out)
}

fn is_operand_end(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Ident(_)
            | Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_)
            | Tok::FStr(_)
            | Tok::RParen
            | Tok::RBracket
            | Tok::True
            | Tok::False
            | Tok::NoneLit
            | Tok::Question
    )
}

fn is_spaced_binary(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Plus
            | Tok::Minus
            | Tok::Star
            | Tok::Slash
            | Tok::Percent
            | Tok::Amp
            | Tok::Pipe
            | Tok::Caret
            | Tok::Shl
            | Tok::Shr
            | Tok::EqEq
            | Tok::NotEq
            | Tok::Lt
            | Tok::Le
            | Tok::Gt
            | Tok::Ge
            | Tok::Assign
            | Tok::PlusEq
            | Tok::MinusEq
            | Tok::StarEq
            | Tok::SlashEq
            | Tok::PercentEq
            | Tok::Arrow
            | Tok::And
            | Tok::Or
            | Tok::In
    )
}

fn render_line(src: &str, toks: &[Token]) -> String {
    let mut s = String::new();
    // Paren/bracket nesting depth, to know whether ':' separates a type annotation.
    let mut nest = 0i32;
    for (i, t) in toks.iter().enumerate() {
        let text = match &t.tok {
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::FStr(_) => {
                src[t.span.lo as usize..t.span.hi as usize].to_string()
            }
            other => other.text().to_string(),
        };
        if i > 0 {
            let prev = &toks[i - 1].tok;
            let cur = &t.tok;
            let prev_unary = matches!(prev, Tok::Minus | Tok::Plus | Tok::Tilde) && (i < 2 || !is_operand_end(&toks[i - 2].tok));
            let space = if matches!(
                cur,
                Tok::Comma | Tok::Colon | Tok::Dot | Tok::RParen | Tok::RBracket | Tok::Question | Tok::DotDot | Tok::DotDotEq
            ) {
                false
            } else if matches!(prev, Tok::Dot | Tok::LParen | Tok::LBracket | Tok::Tilde | Tok::DotDot | Tok::DotDotEq) || prev_unary {
                false
            } else if matches!(cur, Tok::LParen) {
                // call / grouping: no space after a name or closing bracket
                !(is_operand_end(prev) && !matches!(prev, Tok::Str(_) | Tok::FStr(_)) || matches!(prev, Tok::Fn))
                    && !matches!(prev, Tok::Ident(_))
            } else if matches!(cur, Tok::LBracket) {
                !is_operand_end(prev)
            } else if nest > 0 && (matches!(cur, Tok::Assign) || matches!(prev, Tok::Assign)) {
                // keyword argument: name=value
                false
            } else if matches!(prev, Tok::Comma | Tok::Colon) {
                true
            } else if is_spaced_binary(cur) || is_spaced_binary(prev) {
                true
            } else {
                // keywords and names next to each other
                true
            };
            // a unary minus after an operator/keyword still gets a space before it
            if space {
                s.push(' ');
            }
        }
        match &t.tok {
            Tok::LParen | Tok::LBracket => nest += 1,
            Tok::RParen | Tok::RBracket => nest -= 1,
            _ => {}
        }
        s.push_str(&text);
    }
    s
}
