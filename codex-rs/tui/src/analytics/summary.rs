//! Summary year-view interaction.
//! Aggregation changes reuse the loaded profile; account refresh replaces all profile data.

use super::AnalyticsView;
use super::activity_chart::TokenActivityView;
use super::sections::Section;
use crate::keymap::ListAction;

pub(super) const VIEWS: [TokenActivityView; 3] = [
    TokenActivityView::Daily,
    TokenActivityView::Weekly,
    TokenActivityView::Cumulative,
];

impl AnalyticsView {
    pub(crate) fn select_summary(&mut self, view: Option<TokenActivityView>) {
        self.section = Section::Summary;
        self.zoomed = true;
        self.show_help = false;
        *self.scroll_offset_mut() = 0;
        if let Some(view) = view {
            self.sections[Section::Summary].group = VIEWS
                .iter()
                .position(|item| *item == view)
                .unwrap_or_default();
        }
    }

    pub(super) fn summary_action(&mut self, action: Option<ListAction>) {
        let step = match action {
            Some(ListAction::MoveUp) => Some(-1),
            Some(ListAction::MoveDown) => Some(1),
            Some(ListAction::PageUp) => Some(-(self.viewport_height as isize)),
            Some(ListAction::PageDown) => Some(self.viewport_height as isize),
            Some(ListAction::JumpTop) => {
                *self.scroll_offset_mut() = 0;
                Some(0)
            }
            Some(ListAction::JumpBottom) => {
                *self.scroll_offset_mut() = usize::MAX;
                Some(0)
            }
            Some(ListAction::Cancel) => {
                self.is_done = true;
                None
            }
            Some(ListAction::MoveLeft | ListAction::MoveRight | ListAction::Accept) | None => None,
        };
        if let Some(step) = step {
            *self.scroll_offset_mut() = self.scroll_offset().saturating_add_signed(step);
            self.follow_selection = false;
        }
    }
}
