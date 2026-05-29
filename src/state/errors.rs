//! On-screen compositor error bar state. Errors are keyed by source so each
//! source contributes at most one message; the bar shows the union and clears
//! per-source as each problem is resolved (config reload, background reload).

use super::DriftWm;

/// Source of a compositor-generated error shown in the on-screen error bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorSource {
    /// Config file read/parse failure (the old config stays active).
    Config,
    /// Background image load/upload failure (falls back to the default shader).
    Background,
}

impl DriftWm {
    /// Show `message` for `source` in the error bar. No-op (and no redraw) when
    /// that source's message is unchanged.
    pub fn set_error(&mut self, source: ErrorSource, message: impl Into<String>) {
        let message = message.into();
        if self.errors.get(&source) != Some(&message) {
            self.errors.insert(source, message);
            self.mark_all_dirty();
        }
    }

    /// Clear `source`'s error once the problem is resolved. No-op if not set.
    pub fn clear_error(&mut self, source: ErrorSource) {
        if self.errors.remove(&source).is_some() {
            self.mark_all_dirty();
        }
    }
}
