//! The editor's line-number gutter.
//!
//! Drawn UNDER the text editor, inside the editor's own left padding, so that the
//! editor's bounds cover the numbers and a drag that wanders over them keeps
//! extending the selection instead of stopping dead at a strip the pointer
//! naturally crosses. It captures no events, so everything still reaches the
//! editor. Which numbers to draw comes from the editor's own scroll
//! (`editor::gutter_window`) rather than from a copy kept alongside it, so the
//! two columns cannot drift apart.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text;
use iced::advanced::widget::{self, Widget};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Size};

pub struct Gutter {
    numbers: String,
    /// Right edge of the number column, from the left of the editor. The numbers
    /// are right-aligned against it.
    right: f32,
    /// Top of the first number, from the top of the editor. Negative while the
    /// first line is only partly on screen.
    top: f32,
    line_h: f32,
    text_h: f32,
    size: f32,
    font: iced::Font,
    color: Color,
}

impl Gutter {
    pub fn new(
        first: usize,
        count: usize,
        top: f32,
        right: f32,
        line_h: f32,
        size: f32,
        font: iced::Font,
        color: Color,
    ) -> Self {
        let mut numbers = String::with_capacity(count * 5);
        for n in first + 1..=first + count {
            if !numbers.is_empty() {
                numbers.push('\n');
            }
            numbers.push_str(&n.to_string());
        }
        Gutter {
            numbers,
            right,
            top,
            line_h,
            text_h: count as f32 * line_h + 1.0,
            size,
            font,
            color,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Gutter
where
    Renderer: text::Renderer<Font = iced::Font>,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &self,
        _tree: &mut widget::Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn draw(
        &self,
        _tree: &widget::Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if self.numbers.is_empty() {
            return;
        }
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else { return };
        renderer.fill_text(
            text::Text {
                content: self.numbers.clone(),
                bounds: Size::new(self.right, self.text_h),
                size: iced::Pixels(self.size),
                line_height: text::LineHeight::Absolute(iced::Pixels(self.line_h)),
                font: self.font,
                horizontal_alignment: iced::alignment::Horizontal::Right,
                vertical_alignment: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Basic,
                wrapping: text::Wrapping::None,
            },
            Point::new(bounds.x + self.right, bounds.y + self.top),
            self.color,
            clip,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Gutter> for Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer<Font = iced::Font> + 'a,
    Message: 'a,
    Theme: 'a,
{
    fn from(g: Gutter) -> Self {
        Element::new(g)
    }
}
