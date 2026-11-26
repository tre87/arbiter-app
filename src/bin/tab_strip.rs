//! The workspace tabs in the titlebar: as many as fit, then an arrow for the rest,
//! then "+", then whatever fills the remaining width (the window-drag area).
//!
//! When the tabs do not all fit, their names are cut first, all to the same length,
//! down to three letters; only tabs that do not fit even then go behind the arrow
//! (the user's call, 2026-11-24, after one version that only shrank and one that only
//! hid). Both are decided here, in layout, from widths iced has just measured, because
//! the titlebar's other contents are only estimated in the view. What was decided is
//! reported to the app through `on_fit`, which needs it for the arrow's menu, for where
//! a dragged tab would land and for where a new one goes.

use std::cell::Cell;
use std::rc::Rc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{overlay, Clipboard, Shell};
use iced::{alignment, event, mouse, Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Vector};

use super::truncate_name;

/// The shortest a name is cut to, in `truncate_name`'s count: three letters and "…".
pub const NAME_MIN: usize = 4;
/// The longest a name shows, however much room there is.
pub const NAME_MAX: usize = 40;

/// How the strip was laid out last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// Tabs shown, counted from the first; the rest are behind the arrow.
    pub shown: usize,
    /// Tabs in the strip. Part of the fit so that adding or closing a workspace
    /// always reports one, even when `shown` stays the same.
    pub tabs: usize,
    /// The arrow's left edge in window coordinates (where its menu drops down from).
    /// Meaningless while every tab is shown.
    pub arrow_x: f32,
}

impl Fit {
    /// Before the first layout: everything shown.
    pub const UNKNOWN: Fit = Fit { shown: usize::MAX, tabs: 0, arrow_x: 0.0 };
}

/// The length the strip is cutting tab names to, shared with each tab's name. The
/// strip tries lengths in layout, laying the tabs out at each, and keeps the longest
/// that fits; a name reads the length it is being laid out at from here.
#[derive(Clone)]
pub struct Names(Rc<Cell<usize>>);

impl Names {
    pub fn new() -> Self {
        Names(Rc::new(Cell::new(NAME_MAX)))
    }

    /// A tab's name, cut to the length the strip settles on.
    pub fn name<'a, Message: 'a>(&self, name: &str, size: f32) -> Element<'a, Message> {
        Element::new(TabName { name: name.to_string(), size, cap: self.0.clone() })
    }
}

type Paragraph = <iced::Renderer as text::Renderer>::Paragraph;

struct TabName {
    name: String,
    size: f32,
    cap: Rc<Cell<usize>>,
}

struct NameState {
    name: String,
    size: f32,
    /// The name cut to each length the strip has tried, by length (`truncate_name`'s
    /// count; index `len` is the whole name). Kept because the strip lays its tabs out
    /// at several lengths whenever the fit has to be worked out again, a window
    /// resize included, and shaping is the costly part of that.
    shaped: Vec<Option<Paragraph>>,
    /// The length of the last layout, which is the one drawn.
    current: usize,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for TabName {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<NameState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(NameState { name: String::new(), size: 0.0, shaped: Vec::new(), current: 0 })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let st = tree.state.downcast_mut::<NameState>();
        if st.name != self.name || st.size != self.size {
            *st = NameState { name: self.name.clone(), size: self.size, shaped: Vec::new(), current: 0 };
        }
        let cap = self.cap.get().min(self.name.chars().count());
        if st.shaped.len() <= cap {
            st.shaped.resize_with(cap + 1, || None);
        }
        let shaped = st.shaped[cap].get_or_insert_with(|| {
            Paragraph::with_text(text::Text {
                content: &truncate_name(&self.name, cap),
                bounds: Size::INFINITY,
                size: Pixels(self.size),
                line_height: text::LineHeight::default(),
                font: renderer.default_font(),
                horizontal_alignment: alignment::Horizontal::Left,
                vertical_alignment: alignment::Vertical::Top,
                shaping: text::Shaping::Basic,
                wrapping: text::Wrapping::None,
            })
        });
        let bounds = shaped.min_bounds();
        st.current = cap;
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, bounds))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<NameState>();
        if let Some(Some(shaped)) = st.shaped.get(st.current) {
            renderer.fill_paragraph(shaped, layout.bounds().position(), style.text_color, *viewport);
        }
    }
}

pub struct TabStrip<'a, Message> {
    /// The tabs in order, then the arrow, then "+", then the filler.
    children: Vec<Element<'a, Message>>,
    tabs: usize,
    names: Names,
    /// Between two tabs.
    spacing: f32,
    /// Before the arrow, "+" and the filler.
    tail: f32,
    lead: f32,
    /// The insertion gap (0..=shown) to mark while a tab is dragged over the strip.
    gap: Option<usize>,
    gap_color: Color,
    on_fit: fn(Fit) -> Message,
}

impl<'a, Message> TabStrip<'a, Message> {
    /// `names` is what the tabs' names were made with (`Names::name`).
    pub fn new(
        tabs: Vec<Element<'a, Message>>,
        names: &Names,
        arrow: Element<'a, Message>,
        plus: Element<'a, Message>,
        filler: Element<'a, Message>,
        on_fit: fn(Fit) -> Message,
    ) -> Self {
        let n = tabs.len();
        let mut children = tabs;
        children.extend([arrow, plus, filler]);
        TabStrip {
            children,
            tabs: n,
            names: names.clone(),
            spacing: 3.0,
            tail: 6.0,
            lead: 0.0,
            gap: None,
            gap_color: Color::WHITE,
            on_fit,
        }
    }

    /// The space between two tabs, and before what follows them.
    pub fn spacing(mut self, spacing: f32, tail: f32) -> Self {
        self.spacing = spacing;
        self.tail = tail;
        self
    }

    /// Space before the first tab.
    pub fn lead(mut self, lead: f32) -> Self {
        self.lead = lead;
        self
    }

    pub fn gap(mut self, gap: Option<usize>, color: Color) -> Self {
        self.gap = gap;
        self.gap_color = color;
        self
    }

    fn arrow(&self) -> usize {
        self.tabs
    }

    fn visible(&self, i: usize, shown: usize) -> bool {
        visible(self.tabs, i, shown)
    }
}

/// How many of the tabs, `widths` wide, fit in `width` before "+" (and before the arrow
/// when not all of them do). `gaps` is (lead, spacing between tabs, tail before what
/// follows them).
fn tabs_that_fit(widths: &[f32], arrow: f32, plus: f32, gaps: (f32, f32, f32), width: f32) -> usize {
    let (lead, spacing, tail) = gaps;
    let n = widths.len();
    // Tabs 0..k end at `ends[k]`, measured from the lead (0 for none).
    let mut ends = vec![0.0_f32];
    for (i, w) in widths.iter().enumerate() {
        ends.push(ends[i] + if i > 0 { spacing } else { 0.0 } + w);
    }
    let tail = |k: usize| if k > 0 { tail } else { 0.0 };
    if lead + ends[n] + tail(n) + plus <= width {
        return n;
    }
    let after = arrow + gaps.2 + plus;
    (0..n).rev().find(|&k| lead + ends[k] + tail(k) + after <= width).unwrap_or(0)
}

/// The longest name length in `NAME_MIN..=NAME_MAX` that shows as many tabs as the
/// shortest does. `shown_at(len)` is how many tabs fit with their names cut to `len`,
/// which never grows with the length.
fn longest_cap(mut shown_at: impl FnMut(usize) -> usize) -> usize {
    let want = shown_at(NAME_MIN);
    if shown_at(NAME_MAX) >= want {
        return NAME_MAX;
    }
    let (mut good, mut bad) = (NAME_MIN, NAME_MAX);
    while bad - good > 1 {
        let mid = (good + bad) / 2;
        if shown_at(mid) >= want {
            good = mid;
        } else {
            bad = mid;
        }
    }
    good
}

/// Whether child `i` of a strip of `tabs` tabs takes part in the layout: a hidden tab,
/// or the arrow while every tab is shown, is laid out at no size and gets no events or
/// drawing.
fn visible(tabs: usize, i: usize, shown: usize) -> bool {
    if i < tabs {
        i < shown
    } else if i == tabs {
        shown < tabs
    } else {
        true
    }
}

#[derive(Default)]
struct State {
    shown: usize,
    published: Option<Fit>,
    /// The name length chosen last and what it was chosen for: each tab's width with
    /// its whole name, and the strip's, the arrow's and "+"'s widths. Layout runs on
    /// every rebuild of the view, 60 times a second while an agent works, and choosing
    /// lays every tab out at several lengths.
    chosen: Option<(Vec<f32>, [f32; 3], usize)>,
}

impl<'a, Message> Widget<Message, iced::Theme, iced::Renderer> for TabStrip<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let size = limits.max();
        let n = self.tabs;
        let filler = n + 2;
        let loose = layout::Limits::new(Size::ZERO, size);
        let st = tree.state.downcast_mut::<State>();
        let (tab_trees, after) = tree.children.split_at_mut(n);
        let arrow_node = self.children[n].as_widget().layout(&mut after[0], renderer, &loose);
        let plus_node = self.children[n + 1].as_widget().layout(&mut after[1], renderer, &loose);
        let (arrow_w, plus_w) = (arrow_node.size().width, plus_node.size().width);
        let gaps = (self.lead, self.spacing, self.tail);
        let widths = |nodes: &[layout::Node]| nodes.iter().map(|node| node.size().width).collect::<Vec<f32>>();
        let lay_tabs = |trees: &mut [Tree], cap: usize| {
            self.names.0.set(cap);
            self.children[..n]
                .iter()
                .zip(trees.iter_mut())
                .map(|(child, t)| child.as_widget().layout(t, renderer, &loose))
                .collect::<Vec<_>>()
        };

        let whole = lay_tabs(tab_trees, NAME_MAX);
        let key = (widths(&whole), [size.width, arrow_w, plus_w]);
        let known = st.chosen.as_ref().filter(|(w, m, _)| (w, m) == (&key.0, &key.1)).map(|c| c.2);
        // Each name draws the length it was laid out at last, so the tabs are laid out
        // once more at the chosen length unless the whole names were the last.
        let (cap, mut nodes) = match known {
            Some(NAME_MAX) => (NAME_MAX, whole),
            Some(cap) => (cap, lay_tabs(tab_trees, cap)),
            None => {
                let cap = longest_cap(|cap| {
                    tabs_that_fit(&widths(&lay_tabs(tab_trees, cap)), arrow_w, plus_w, gaps, size.width)
                });
                (cap, lay_tabs(tab_trees, cap))
            }
        };
        let shown = tabs_that_fit(&widths(&nodes), arrow_w, plus_w, gaps, size.width);
        st.shown = shown;
        st.chosen = Some((key.0, key.1, cap));
        nodes.extend([arrow_node, plus_node]);

        let mut x = self.lead;
        let mut first = true;
        for (i, node) in nodes.iter_mut().enumerate() {
            if !self.visible(i, shown) {
                *node = layout::Node::new(Size::ZERO);
                continue;
            }
            if !first {
                x += if i < n { self.spacing } else { self.tail };
            }
            first = false;
            let s = node.size();
            node.move_to_mut(Point::new(x, ((size.height - s.height) / 2.0).max(0.0)));
            x += s.width;
        }
        x += self.tail;
        let rest = (size.width - x).max(0.0);
        let filler_node = self.children[filler]
            .as_widget()
            .layout(&mut tree.children[filler], renderer, &layout::Limits::new(Size::ZERO, Size::new(rest, size.height)))
            .move_to(Point::new(x, 0.0));
        nodes.push(filler_node);
        layout::Node::with_children(size, nodes)
    }

    fn operate(&self, tree: &mut Tree, layout: Layout<'_>, renderer: &iced::Renderer, operation: &mut dyn Operation) {
        let shown = tree.state.downcast_ref::<State>().shown;
        operation.container(None, layout.bounds(), &mut |operation| {
            for (i, ((child, state), layout)) in
                self.children.iter().zip(&mut tree.children).zip(layout.children()).enumerate()
            {
                if self.visible(i, shown) {
                    child.as_widget().operate(state, layout, renderer, operation);
                }
            }
        });
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        // Every layout is followed by a redraw, so this is where a new fit is first seen.
        if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
            let st = tree.state.downcast_mut::<State>();
            let arrow_x = layout.children().nth(self.arrow()).map_or(0.0, |l| l.bounds().x);
            let fit = Fit { shown: st.shown, tabs: self.tabs, arrow_x };
            if st.published != Some(fit) {
                st.published = Some(fit);
                shell.publish((self.on_fit)(fit));
            }
        }
        let shown = tree.state.downcast_ref::<State>().shown;
        let tabs = self.tabs;
        let mut status = event::Status::Ignored;
        for (i, ((child, state), layout)) in
            self.children.iter_mut().zip(&mut tree.children).zip(layout.children()).enumerate()
        {
            if !visible(tabs, i, shown) {
                continue;
            }
            status = status.merge(child.as_widget_mut().on_event(
                state,
                event.clone(),
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            ));
        }
        status
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let shown = tree.state.downcast_ref::<State>().shown;
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .enumerate()
            .filter(|(i, _)| self.visible(*i, shown))
            .map(|(_, ((child, state), layout))| {
                child.as_widget().mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else { return };
        let shown = tree.state.downcast_ref::<State>().shown;
        let tabs: Vec<Rectangle> = layout.children().take(shown).map(|l| l.bounds()).collect();
        for (i, ((child, state), layout)) in self.children.iter().zip(&tree.children).zip(layout.children()).enumerate()
        {
            if self.visible(i, shown) {
                child.as_widget().draw(state, renderer, theme, style, layout, cursor, &clip);
            }
        }
        // The drop mark sits in the gap between two tabs without taking room of its
        // own: an inserted element moved the tabs under the cursor, and with them
        // which tab it was over.
        if let Some(g) = self.gap.filter(|&g| g <= shown && !tabs.is_empty()) {
            let x = if g < tabs.len() {
                tabs[g].x - self.spacing / 2.0
            } else {
                tabs[tabs.len() - 1].x + tabs[tabs.len() - 1].width + self.spacing / 2.0
            };
            let h = 22.0_f32.min(bounds.height);
            <iced::Renderer as iced::advanced::Renderer>::fill_quad(
                renderer,
                renderer::Quad {
                    bounds: Rectangle { x: x - 1.0, y: bounds.y + (bounds.height - h) / 2.0, width: 2.0, height: h },
                    border: iced::Border { radius: 1.0.into(), ..Default::default() },
                    ..Default::default()
                },
                self.gap_color,
            );
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        // Not `overlay::from_children`: a hidden child's node has no children of its
        // own, and a button asked for its overlay unwraps its content's node.
        let shown = tree.state.downcast_ref::<State>().shown;
        let tabs = self.tabs;
        let overlays: Vec<_> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .enumerate()
            .filter(|(i, _)| visible(tabs, *i, shown))
            .filter_map(|(_, ((child, state), layout))| {
                child.as_widget_mut().overlay(state, layout, renderer, translation)
            })
            .collect();
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
    }
}

impl<'a, Message: 'a> From<TabStrip<'a, Message>> for Element<'a, Message> {
    fn from(strip: TabStrip<'a, Message>) -> Self {
        Element::new(strip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // lead 8, 3 between tabs, 6 before the arrow and "+"; arrow 52, "+" 26.
    const GAPS: (f32, f32, f32) = (8.0, 3.0, 6.0);

    #[test]
    fn every_tab_shows_while_they_fit_with_plus() {
        // 8 + 100 + 3 + 100 + 6 + 26 = 243.
        assert_eq!(tabs_that_fit(&[100.0, 100.0], 52.0, 26.0, GAPS, 243.0), 2);
        assert_eq!(tabs_that_fit(&[], 52.0, 26.0, GAPS, 10.0), 0);
    }

    #[test]
    fn the_rest_go_behind_the_arrow_which_needs_room_of_its_own() {
        // One short of all fitting: the arrow then has to fit too, so a second tab
        // goes as well. 8 + 100 + 6 + 52 + 6 + 26 = 198.
        assert_eq!(tabs_that_fit(&[100.0, 100.0], 52.0, 26.0, GAPS, 242.0), 1);
        assert_eq!(tabs_that_fit(&[100.0, 100.0, 100.0], 52.0, 26.0, GAPS, 198.0), 1);
        assert_eq!(tabs_that_fit(&[100.0, 100.0, 100.0], 52.0, 26.0, GAPS, 197.0), 0);
        // Tabs keep their order: a narrow third tab does not jump a wide second one.
        assert_eq!(tabs_that_fit(&[100.0, 300.0, 20.0], 52.0, 26.0, GAPS, 300.0), 1);
    }

    #[test]
    fn names_shorten_before_any_tab_goes_behind_the_arrow() {
        // Ten tabs of 12-letter names, 20 px plus 6 px a character at the length
        // they are cut to.
        let shown_at = |width: f32| {
            move |cap: usize| {
                let w = 20.0 + 6.0 * cap.min(12) as f32;
                tabs_that_fit(&[w; 10], 52.0, 26.0, GAPS, width)
            }
        };
        assert_eq!(longest_cap(shown_at(2000.0)), NAME_MAX, "every whole name fits");
        // All ten fit while 8 + 10w + 27 + 6 + 26 <= 700, so w <= 63.3: length 7.
        assert_eq!(longest_cap(shown_at(700.0)), 7);
        // k tabs and the arrow fit while 95 + k(w + 3) <= width. At the shortest
        // length (w 44) that is 6 tabs in 400 px, and at length 5 (w 50) only 5.
        assert_eq!(shown_at(400.0)(NAME_MIN), 6);
        assert_eq!(longest_cap(shown_at(400.0)), NAME_MIN);
        // In 420 px still 6 at length 5, but 5 at length 6 (w 56): the names get the
        // room the arrow leaves.
        assert_eq!(longest_cap(shown_at(420.0)), 5);
    }

    #[test]
    fn hidden_tabs_and_an_unneeded_arrow_take_no_part() {
        assert!(visible(3, 1, 2) && !visible(3, 2, 2));
        assert!(visible(3, 3, 2), "the arrow shows while a tab is hidden");
        assert!(!visible(3, 3, 3), "and not once all of them show");
        assert!(visible(3, 4, 0) && visible(3, 5, 0), "\"+\" and the filler always show");
    }
}
