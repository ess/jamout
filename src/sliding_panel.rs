//! Reusable Composite Sliding Panel Stack for egui.
//!
//! Provides a composable, arbitrary-depth docking inspector architecture:
//! - Root stacks anchor to viewport edges (e.g. docked to left toolbar).
//! - Child stacks can anchor directly to any parent panel's bounding rectangle (`StackAnchor::ParentRect`).
//! - Automatic coordinate alignment across horizontal slide-outs or vertical drawers.
//! - Cascading dismissal and clean component encapsulation.

use egui::{Align2, Context, Id, Rect, Ui, Vec2, Window};

#[allow(dead_code)]
/// The direction adjacent panels slide out towards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideDirection {
    /// Slides out horizontally towards the right
    Horizontal,
    /// Slides out vertically towards the bottom
    Vertical,
}

/// The origin anchor point for a panel stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StackAnchor {
    /// Anchored to viewport coordinates (e.g., Align2::LEFT_TOP with pixel offset)
    Screen(Align2, Vec2),
    /// Anchored flush to a parent panel's bounding rectangle
    ParentRect(Rect),
}

/// A composable stack of sliding, immovable docked inspector panels.
pub struct SlidingPanelStack {
    pub anchor: StackAnchor,
    pub direction: SlideDirection,
    pub gap: f32,
    pub default_width: f32,
    pub movable: bool,
    pub resizable: bool,
    pub collapsible: bool,
    last_rect: Option<Rect>,
}

impl SlidingPanelStack {
    /// Create a root panel stack docked to the top-left of the screen.
    pub fn docked_left(top_offset: f32) -> Self {
        Self {
            anchor: StackAnchor::Screen(Align2::LEFT_TOP, Vec2::new(12.0, top_offset)),
            direction: SlideDirection::Horizontal,
            gap: 8.0,
            default_width: 340.0,
            movable: false,
            resizable: true,
            collapsible: false,
            last_rect: None,
        }
    }

    /// Create a child panel stack nested and docked directly to a parent panel's rect.
    pub fn from_parent(parent_rect: Rect) -> Self {
        Self {
            anchor: StackAnchor::ParentRect(parent_rect),
            direction: SlideDirection::Horizontal,
            gap: 8.0,
            default_width: 340.0,
            movable: false,
            resizable: true,
            collapsible: false,
            last_rect: None,
        }
    }

    #[allow(dead_code)]
    /// Configure the slide direction (Horizontal for slide-outs, Vertical for drawers).
    pub fn with_direction(mut self, direction: SlideDirection) -> Self {
        self.direction = direction;
        self
    }

    #[allow(dead_code)]
    /// Configure the default panel width in pixels.
    pub fn with_default_width(mut self, width: f32) -> Self {
        self.default_width = width;
        self
    }

    #[allow(dead_code)]
    /// Configure the gap between adjacent panels in pixels.
    pub fn with_gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Calculate the anchor coordinates for the next panel to be displayed.
    pub fn next_anchor_pos(&self) -> (Align2, Vec2) {
        match (self.last_rect, self.anchor) {
            (Some(prev), _) => match self.direction {
                SlideDirection::Horizontal => (
                    Align2::LEFT_TOP,
                    Vec2::new(prev.right() + self.gap, prev.top()),
                ),
                SlideDirection::Vertical => (
                    Align2::LEFT_TOP,
                    Vec2::new(prev.left(), prev.bottom() + self.gap),
                ),
            },
            (None, StackAnchor::Screen(align, offset)) => (align, offset),
            (None, StackAnchor::ParentRect(parent)) => match self.direction {
                SlideDirection::Horizontal => (
                    Align2::LEFT_TOP,
                    Vec2::new(parent.right() + self.gap, parent.top()),
                ),
                SlideDirection::Vertical => (
                    Align2::LEFT_TOP,
                    Vec2::new(parent.left(), parent.bottom() + self.gap),
                ),
            },
        }
    }

    /// Render a panel in this stack.
    ///
    /// Returns `Some(Rect)` with the panel's bounding rectangle if open and rendered,
    /// which can be used to spawn child `SlidingPanelStack::from_parent(rect)` instances.
    pub fn panel<R>(
        &mut self,
        ctx: &Context,
        id: Id,
        title: impl Into<egui::WidgetText>,
        is_open: &mut bool,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<Rect> {
        if !*is_open {
            return None;
        }

        let (align, offset) = self.next_anchor_pos();

        let inner_resp = Window::new(title)
            .id(id)
            .open(is_open)
            .resizable(self.resizable)
            .movable(self.movable)
            .collapsible(self.collapsible)
            .default_width(self.default_width)
            .anchor(align, offset)
            .show(ctx, |ui| add_contents(ui));

        if let Some(resp) = inner_resp {
            let rect = resp.response.rect;
            self.last_rect = Some(rect);
            Some(rect)
        } else {
            None
        }
    }
}
