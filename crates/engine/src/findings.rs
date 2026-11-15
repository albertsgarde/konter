//! Findings: what a build or check found that needs your attention (docs/spec.md §7).

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    /// A check that passed.
    Ok,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub severity: Severity,
    pub category: &'static str,
    pub message: String,
    pub details: Vec<String>,
}

#[derive(Default, Debug)]
pub struct Findings {
    pub diags: Vec<Diag>,
}

/// Lists longer than this are cut short when printed.
const MAX_DETAILS: usize = 25;

impl Findings {
    pub fn push(&mut self, severity: Severity, category: &'static str, message: impl Into<String>) -> &mut Diag {
        self.diags.push(Diag { severity, category, message: message.into(), details: Vec::new() });
        self.diags.last_mut().unwrap()
    }

    pub fn error(&mut self, category: &'static str, message: impl Into<String>) -> &mut Diag {
        self.push(Severity::Error, category, message)
    }

    pub fn warn(&mut self, category: &'static str, message: impl Into<String>) -> &mut Diag {
        self.push(Severity::Warning, category, message)
    }

    pub fn info(&mut self, category: &'static str, message: impl Into<String>) -> &mut Diag {
        self.push(Severity::Info, category, message)
    }

    pub fn ok(&mut self, category: &'static str, message: impl Into<String>) -> &mut Diag {
        self.push(Severity::Ok, category, message)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.diags.iter().filter(|d| d.severity == severity).count()
    }

    pub fn has_errors(&self) -> bool {
        self.count(Severity::Error) > 0
    }

    pub fn extend(&mut self, other: Findings) {
        self.diags.extend(other.diags);
    }

    /// Renders the findings, most severe first. `full` prints every detail line.
    pub fn render(&self, full: bool) -> String {
        let mut diags: Vec<&Diag> = self.diags.iter().collect();
        diags.sort_by_key(|d| d.severity); // stable: keeps insertion order within a severity
        let mut out = String::new();
        for d in diags {
            let symbol = match d.severity {
                Severity::Error => "✗",
                Severity::Warning => "⚠",
                Severity::Info => "·",
                Severity::Ok => "✓",
            };
            let _ = writeln!(out, "{symbol} {:<18} {}", d.category, d.message);
            let shown = if full { d.details.len() } else { d.details.len().min(MAX_DETAILS) };
            for line in &d.details[..shown] {
                let _ = writeln!(out, "      {line}");
            }
            if shown < d.details.len() {
                let _ = writeln!(out, "      … and {} more", d.details.len() - shown);
            }
        }
        let _ = writeln!(out, "\n{} errors, {} warnings", self.count(Severity::Error), self.count(Severity::Warning));
        out
    }
}

impl Diag {
    pub fn detail(&mut self, line: impl Into<String>) -> &mut Diag {
        self.details.push(line.into());
        self
    }
}
