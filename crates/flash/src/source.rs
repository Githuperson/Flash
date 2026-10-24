//! Source files, spans and the source map.

use std::path::PathBuf;

/// A byte range inside one source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: u32,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: u32, lo: usize, hi: usize) -> Span {
        Span { file, lo: lo as u32, hi: hi as u32 }
    }
    /// Smallest span covering both `self` and `other` (must be in the same file).
    pub fn to(self, other: Span) -> Span {
        Span { file: self.file, lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
}

#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    /// Name shown in diagnostics.
    pub display: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(path: PathBuf, display: String, text: String) -> SourceFile {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceFile { path, display, text, line_starts }
    }

    /// 0-based line index containing byte offset `pos`.
    pub fn line_of(&self, pos: usize) -> usize {
        match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line];
        let end = self.line_starts.get(line + 1).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts[line]
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// (1-based line, 1-based column in characters)
    pub fn line_col(&self, pos: usize) -> (usize, usize) {
        let line = self.line_of(pos);
        let start = self.line_starts[line];
        let pos = pos.min(self.text.len());
        let col = self.text[start..pos].chars().count() + 1;
        (line + 1, col)
    }
}

#[derive(Default, Debug)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, path: PathBuf, display: String, text: String) -> u32 {
        self.files.push(SourceFile::new(path, display, text));
        (self.files.len() - 1) as u32
    }
    pub fn get(&self, id: u32) -> &SourceFile {
        &self.files[id as usize]
    }
}
