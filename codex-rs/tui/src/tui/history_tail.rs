//! Update visible terminal-history tails without discarding terminal scrollback.
//! When the existing tail is an unchanged prefix, append only the new rows.

use super::Tui;

#[cfg(test)]
use crate::terminal_hyperlinks::HyperlinkLine;

impl Tui {
    #[cfg(test)]
    pub(crate) fn pending_history_lines_for_test(&self) -> Vec<HyperlinkLine> {
        self.pending_history_lines
            .iter()
            .flat_map(|batch| batch.lines.iter().cloned())
            .collect()
    }
}
