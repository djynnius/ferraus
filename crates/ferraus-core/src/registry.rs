//! The registry of every op, assembled by the CLI from each crate's `ops()`.

use crate::error::{Error, Result};
use crate::op::OpSpec;

#[derive(Default)]
pub struct Registry {
    ops: Vec<OpSpec>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Add ops. Panics on a duplicate name: duplicates are a programming error caught by tests.
    pub fn extend(&mut self, ops: impl IntoIterator<Item = OpSpec>) {
        for op in ops {
            assert!(self.ops.iter().all(|o| o.name != op.name), "duplicate op registered: {}", op.name);
            self.ops.push(op);
        }
    }

    pub fn ops(&self) -> &[OpSpec] {
        &self.ops
    }

    /// Look up by command path (`denoise profile`), id (`denoise_profile`), or alias.
    pub fn get(&self, name: &str) -> Option<&OpSpec> {
        let norm = name.trim().replace(['_', '.'], " ");
        self.ops.iter().find(|o| o.name == name || o.name == norm || o.id() == name || o.aliases.contains(&name))
    }

    pub fn require(&self, name: &str) -> Result<&OpSpec> {
        self.get(name).ok_or_else(|| {
            let suggestions = self.suggest(name);
            let mut e = Error::usage(format!("unknown command '{name}'"));
            if let Some(s) = suggestions.first() {
                e = e.with_hint(format!("did you mean 'ferraus {s}'? Run 'ferraus schema' to list commands"));
            } else {
                e = e.with_hint("ferraus schema");
            }
            e
        })
    }

    /// Closest command names by edit distance.
    pub fn suggest(&self, name: &str) -> Vec<&'static str> {
        let mut scored: Vec<(usize, &'static str)> =
            self.ops.iter().map(|o| (levenshtein(name, o.name), o.name)).filter(|(d, _)| *d <= 3).collect();
        scored.sort();
        scored.into_iter().map(|(_, n)| n).take(3).collect()
    }

    /// Ops usable as `fx` stages.
    pub fn transforms(&self) -> impl Iterator<Item = &OpSpec> {
        self.ops.iter().filter(|o| o.is_transform())
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
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
