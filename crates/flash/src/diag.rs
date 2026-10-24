//! Diagnostics: errors with source excerpts and suggestions.

use crate::source::{SourceMap, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Option<Span>,
    /// Short text printed under the caret line.
    pub label: Option<String>,
    /// Extra advice ("Help: ...").
    pub help: Option<String>,
    /// Secondary locations ("note: first defined here").
    pub notes: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic { severity: Severity::Error, message: message.into(), span: Some(span), label: None, help: None, notes: vec![] }
    }
    pub fn error_nospan(message: impl Into<String>) -> Diagnostic {
        Diagnostic { severity: Severity::Error, message: message.into(), span: None, label: None, help: None, notes: vec![] }
    }
    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(span, message) }
    }
    pub fn label(mut self, l: impl Into<String>) -> Diagnostic {
        self.label = Some(l.into());
        self
    }
    pub fn help(mut self, h: impl Into<String>) -> Diagnostic {
        self.help = Some(h.into());
        self
    }
    pub fn note(mut self, span: Span, n: impl Into<String>) -> Diagnostic {
        self.notes.push((span, n.into()));
        self
    }
}

/// Collects diagnostics during a compilation.
#[derive(Default, Debug)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, d: Diagnostic) {
        // Avoid reporting the same problem at the same place twice.
        if !self.items.iter().any(|x| x.message == d.message && x.span == d.span) {
            self.items.push(d);
        }
    }
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }
    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Error).count()
    }
}

fn excerpt(map: &SourceMap, span: Span, label: Option<&str>, out: &mut String) {
    let f = map.get(span.file);
    let lo = (span.lo as usize).min(f.text.len());
    let hi = (span.hi as usize).min(f.text.len()).max(lo);
    let first = f.line_of(lo);
    let last = f.line_of(if hi > lo { hi - 1 } else { hi });
    let width = (last + 1).to_string().len().max(3);
    let shown_last = last.min(first + 3);
    for line in first..=shown_last {
        let text = f.line_text(line);
        let text_show = text.replace('\t', "    ");
        out.push_str(&format!("{:>w$} | {}\n", line + 1, text_show, w = width));
        let ls = f.line_start(line);
        let col_lo = if line == first { f.text[ls..lo].chars().count() } else { 0 };
        let line_chars = text.chars().count();
        let col_hi = if line == last {
            let end = hi.min(ls + text.len()).max(ls);
            f.text[ls..end].chars().count()
        } else {
            line_chars
        };
        let n = col_hi.saturating_sub(col_lo).max(1);
        // Tabs were expanded to 4 spaces in the shown text; adjust the caret.
        let tabs_before = text.chars().take(col_lo).filter(|c| *c == '\t').count();
        let pad = col_lo + tabs_before * 3;
        out.push_str(&format!("{:>w$} | {}{}", "", " ".repeat(pad), "^".repeat(n), w = width));
        if line == shown_last {
            if let Some(l) = label {
                out.push(' ');
                out.push_str(l);
            }
        }
        out.push('\n');
    }
    if shown_last < last {
        out.push_str(&format!("{:>w$} | ...\n", "", w = width));
    }
}

/// Render one diagnostic as beginner-friendly text.
pub fn render(map: &SourceMap, d: &Diagnostic) -> String {
    let mut out = String::new();
    let kind = match d.severity {
        Severity::Error => "Error",
        Severity::Warning => "Warning",
    };
    match d.span {
        Some(sp) => {
            let f = map.get(sp.file);
            let (line, col) = f.line_col(sp.lo as usize);
            out.push_str(&format!("{} in {}, line {}, column {}:\n", kind, f.display, line, col));
            out.push_str(&d.message);
            out.push_str("\n\n");
            excerpt(map, sp, d.label.as_deref(), &mut out);
        }
        None => {
            out.push_str(&format!("{}: {}\n", kind, d.message));
        }
    }
    for (sp, note) in &d.notes {
        let f = map.get(sp.file);
        let (line, _) = f.line_col(sp.lo as usize);
        out.push_str(&format!("\nNote: {} ({}, line {})\n", note, f.display, line));
        excerpt(map, *sp, None, &mut out);
    }
    if let Some(h) = &d.help {
        out.push_str(&format!("\nHelp: {}\n", h));
    }
    out
}

pub fn render_all(map: &SourceMap, ds: &Diagnostics) -> String {
    let mut s = String::new();
    for d in &ds.items {
        s.push_str(&render(map, d));
        s.push('\n');
    }
    let n = ds.error_count();
    if n > 0 {
        s.push_str(&format!("Stopped: {} error{} found.\n", n, if n == 1 { "" } else { "s" }));
    }
    s
}

/// Levenshtein-based "did you mean" suggestion.
pub fn suggest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for c in candidates {
        let d = edit_distance(name, c);
        let limit = (name.len() / 3).max(1);
        if d <= limit && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, c.to_string()));
        }
    }
    best.map(|(_, s)| s)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}
