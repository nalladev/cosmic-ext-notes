// SPDX-License-Identifier: MPL-2.0

//! A mouse area for the sidebar's empty-area context menu.
//!
//! The stock iced [`MouseArea`] cannot report the cursor position of a right
//! click, which is why the sidebar used to track cursor movement and republish
//! every move as a message — one full app update, view rebuild and redraw per
//! hover move. This widget reports the position of the right click itself, so
//! no per-move tracking is needed.
//!
//! It mirrors the pattern cosmic-files uses (`on_right_press` with the cursor
//! position), written against the MPL-2.0 iced `MouseArea` rather than
//! cosmic-files' GPL-3.0 implementation.

use cosmic::Element;
use cosmic::iced::core::event::Event;
use cosmic::iced::core::layout;
use cosmic::iced::core::mouse;
use cosmic::iced::core::overlay;
use cosmic::iced::core::renderer;
use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{
    Clipboard, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget,
};

/// Emits a message on right clicks, carrying the cursor position at the
/// release, and an optional message on left presses.
#[must_use]
pub struct SidebarMouseArea<'a, Message> {
    content: Element<'a, Message>,
    on_press: Option<Message>,
    on_right_release: Option<Box<dyn Fn(Point) -> Message + 'a>>,
}

impl<'a, Message> SidebarMouseArea<'a, Message> {
    /// Wraps the given content.
    pub fn new(content: impl Into<Element<'a, Message>>) -> Self {
        Self {
            content: content.into(),
            on_press: None,
            on_right_release: None,
        }
    }

    /// The message to emit on a left button press inside the area.
    pub fn on_press(mut self, message: Message) -> Self {
        self.on_press = Some(message);
        self
    }

    /// The message to emit on a right button release inside the area, with
    /// the cursor position relative to the area's bounds.
    pub fn on_right_release(mut self, on_right_release: impl Fn(Point) -> Message + 'a) -> Self {
        self.on_right_release = Some(Box::new(on_right_release));
        self
    }
}

impl<'a, Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for SidebarMouseArea<'a, Message>
where
    Message: 'a + Clone,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // The wrapped content handles its own events first, capturing any it
        // consumes (like the nav bar's item context menus).
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if shell.is_event_captured() {
            return;
        }

        // Ignore events outside this area.
        if !cursor.is_over(layout.bounds()) {
            return;
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(on_press) = self.on_press.as_ref() {
                    shell.publish(on_press.clone());
                }
                // Let the wrapped content handle the press, but stop it from
                // reaching widgets behind the sidebar.
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)) => {
                if let Some(on_right_release) = self.on_right_release.as_ref()
                    && let Some(position) = cursor.position_in(layout.bounds())
                {
                    shell.publish(on_right_release(position));
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, cosmic::Theme, cosmic::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message> From<SidebarMouseArea<'a, Message>> for Element<'a, Message>
where
    Message: 'a + Clone,
{
    fn from(area: SidebarMouseArea<'a, Message>) -> Element<'a, Message> {
        Element::new(area)
    }
}
