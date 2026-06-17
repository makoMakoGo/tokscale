use std::ops::Range;

use ratatui::layout::Rect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InteractionOutcome {
    Handled,
    Ignored(&'static str),
    Close,
    NeedsReload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ListInteraction {
    pub selected: usize,
    pub scroll: usize,
    pub visible: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveCommand {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrapMode {
    Wrap,
    Clamp,
}

impl ListInteraction {
    pub(crate) fn set_visible(&mut self, visible: usize, len: usize) {
        self.visible = visible.max(1);
        self.clamp(len);
    }

    pub(crate) fn clamp(&mut self, len: usize) {
        self.visible = self.visible.max(1);

        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }

        self.selected = self.selected.min(len - 1);
        self.keep_selected_visible(len);
    }

    pub(crate) fn apply_move(
        &mut self,
        command: MoveCommand,
        len: usize,
        wrap: WrapMode,
    ) -> InteractionOutcome {
        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
            return InteractionOutcome::Ignored("empty list");
        }

        self.clamp(len);
        let old = (self.selected, self.scroll);
        let jump = (self.visible / 2).max(1);
        let max_index = len - 1;

        match command {
            MoveCommand::Up => {
                if self.selected == 0 {
                    match wrap {
                        WrapMode::Wrap => self.selected = max_index,
                        WrapMode::Clamp => {}
                    }
                } else {
                    self.selected -= 1;
                }
            }
            MoveCommand::Down => {
                if self.selected == max_index {
                    match wrap {
                        WrapMode::Wrap => self.selected = 0,
                        WrapMode::Clamp => {}
                    }
                } else {
                    self.selected += 1;
                }
            }
            MoveCommand::PageUp => {
                self.selected = self.selected.saturating_sub(jump);
            }
            MoveCommand::PageDown => {
                self.selected = self.selected.saturating_add(jump).min(max_index);
            }
            MoveCommand::Home => {
                self.selected = 0;
            }
            MoveCommand::End => {
                self.selected = max_index;
            }
        }

        self.keep_selected_visible(len);

        if old == (self.selected, self.scroll) {
            InteractionOutcome::Ignored("at boundary")
        } else {
            InteractionOutcome::Handled
        }
    }

    pub(crate) fn visible_range(&self, len: usize) -> Range<usize> {
        if len == 0 {
            return 0..0;
        }

        let visible = self.visible.max(1);
        let start = self.scroll.min(len.saturating_sub(visible));
        let end = start.saturating_add(visible).min(len);
        start..end
    }

    pub(crate) fn select(&mut self, index: usize, len: usize) -> InteractionOutcome {
        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
            return InteractionOutcome::Ignored("empty list");
        }
        if index >= len {
            self.clamp(len);
            return InteractionOutcome::Ignored("index out of bounds");
        }

        self.selected = index;
        self.keep_selected_visible(len);
        InteractionOutcome::Handled
    }

    fn keep_selected_visible(&mut self, len: usize) {
        let visible = self.visible.max(1);

        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll.saturating_add(visible) {
            self.scroll = self.selected.saturating_sub(visible - 1);
        }

        self.scroll = self.scroll.min(len.saturating_sub(visible));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TextViewport {
    pub scroll: usize,
    pub visible: usize,
}

impl TextViewport {
    pub(crate) fn set_visible(&mut self, visible: usize, total_lines: usize) {
        self.visible = visible.max(1);
        self.clamp(total_lines);
    }

    pub(crate) fn apply_move(
        &mut self,
        command: MoveCommand,
        total_lines: usize,
    ) -> InteractionOutcome {
        if total_lines == 0 {
            self.scroll = 0;
            self.visible = self.visible.max(1);
            return InteractionOutcome::Ignored("empty text");
        }

        self.clamp(total_lines);
        let old = self.scroll;
        let max_scroll = total_lines.saturating_sub(self.visible.max(1));
        let jump = (self.visible.max(1) / 2).max(1);

        self.scroll = match command {
            MoveCommand::Up => self.scroll.saturating_sub(1),
            MoveCommand::Down => self.scroll.saturating_add(1).min(max_scroll),
            MoveCommand::PageUp => self.scroll.saturating_sub(jump),
            MoveCommand::PageDown => self.scroll.saturating_add(jump).min(max_scroll),
            MoveCommand::Home => 0,
            MoveCommand::End => max_scroll,
        };

        if old == self.scroll {
            InteractionOutcome::Ignored("at boundary")
        } else {
            InteractionOutcome::Handled
        }
    }

    pub(crate) fn visible_range(&self, total_lines: usize) -> Range<usize> {
        if total_lines == 0 {
            return 0..0;
        }

        let visible = self.visible.max(1);
        let start = self.scroll.min(total_lines.saturating_sub(visible));
        let end = start.saturating_add(visible).min(total_lines);
        start..end
    }

    fn clamp(&mut self, total_lines: usize) {
        self.visible = self.visible.max(1);
        self.scroll = self.scroll.min(total_lines.saturating_sub(self.visible));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowHitbox {
    pub rect: Rect,
    pub index: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HitMap {
    rows: Vec<RowHitbox>,
}

impl HitMap {
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
    }

    pub(crate) fn push_row(&mut self, rect: Rect, index: usize) {
        self.rows.push(RowHitbox { rect, index });
    }

    pub(crate) fn hit(&self, column: u16, row: u16) -> Option<usize> {
        self.rows
            .iter()
            .find(|hitbox| {
                column >= hitbox.rect.x
                    && column < hitbox.rect.x.saturating_add(hitbox.rect.width)
                    && row >= hitbox.rect.y
                    && row < hitbox.rect.y.saturating_add(hitbox.rect.height)
            })
            .map(|hitbox| hitbox.index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_interaction_wraps_up_down_and_clamps_scroll() {
        let mut state = ListInteraction {
            selected: 0,
            scroll: 0,
            visible: 3,
        };

        assert_eq!(
            state.apply_move(MoveCommand::Up, 10, WrapMode::Wrap),
            InteractionOutcome::Handled
        );
        assert_eq!(state.selected, 9);
        assert_eq!(state.scroll, 7);

        assert_eq!(
            state.apply_move(MoveCommand::Down, 10, WrapMode::Wrap),
            InteractionOutcome::Handled
        );
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll, 0);

        state.selected = 99;
        state.scroll = 99;
        state.set_visible(4, 6);
        assert_eq!(state.selected, 5);
        assert_eq!(state.scroll, 2);
    }

    #[test]
    fn list_interaction_page_moves_keep_selection_visible() {
        let mut state = ListInteraction {
            selected: 0,
            scroll: 0,
            visible: 6,
        };

        assert_eq!(
            state.apply_move(MoveCommand::PageDown, 20, WrapMode::Wrap),
            InteractionOutcome::Handled
        );
        assert_eq!(state.selected, 3);
        assert_eq!(state.visible_range(20), 0..6);

        assert_eq!(
            state.apply_move(MoveCommand::PageDown, 20, WrapMode::Wrap),
            InteractionOutcome::Handled
        );
        assert_eq!(state.selected, 6);
        assert_eq!(state.visible_range(20), 1..7);

        assert_eq!(
            state.apply_move(MoveCommand::PageUp, 20, WrapMode::Wrap),
            InteractionOutcome::Handled
        );
        assert_eq!(state.selected, 3);
        assert_eq!(state.visible_range(20), 1..7);
    }

    #[test]
    fn text_viewport_scrolls_and_clamps_consistently() {
        let mut viewport = TextViewport {
            scroll: 0,
            visible: 4,
        };

        assert_eq!(
            viewport.apply_move(MoveCommand::Down, 10),
            InteractionOutcome::Handled
        );
        assert_eq!(viewport.scroll, 1);

        assert_eq!(
            viewport.apply_move(MoveCommand::PageDown, 10),
            InteractionOutcome::Handled
        );
        assert_eq!(viewport.scroll, 3);

        assert_eq!(
            viewport.apply_move(MoveCommand::End, 10),
            InteractionOutcome::Handled
        );
        assert_eq!(viewport.scroll, 6);
        assert_eq!(viewport.visible_range(10), 6..10);

        viewport.set_visible(20, 10);
        assert_eq!(viewport.scroll, 0);
        assert_eq!(viewport.visible_range(10), 0..10);
    }

    #[test]
    fn hitmap_uses_half_open_rect_boundaries() {
        let mut hitmap = HitMap::default();
        hitmap.push_row(Rect::new(10, 5, 6, 2), 7);

        assert_eq!(hitmap.hit(10, 5), Some(7));
        assert_eq!(hitmap.hit(15, 6), Some(7));
        assert_eq!(hitmap.hit(16, 6), None);
        assert_eq!(hitmap.hit(15, 7), None);

        hitmap.clear();
        assert_eq!(hitmap.hit(10, 5), None);
    }
}
