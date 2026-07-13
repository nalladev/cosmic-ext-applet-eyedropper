// SPDX-License-Identifier: MPL-2.0

//! A widget wrapper that intercepts keyboard events.
//!
//! Adapted from `xdg-desktop-portal-cosmic`'s `KeyboardWrapper`
//! (MIT-licensed) to match the iced Widget trait in this version
//! of libcosmic.

use cosmic::iced::core::event::Event;
use cosmic::iced::core::keyboard;
use cosmic::iced::core::layout::{self, Node};
use cosmic::iced::core::mouse;
use cosmic::iced::core::overlay;
use cosmic::iced::core::renderer;
use cosmic::iced::core::widget::{Operation, Tree};
use cosmic::iced::core::{Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Widget};

#[allow(missing_debug_implementations)]
pub struct KeyboardWrapper<'a, Message> {
    content: Element<'a, Message, cosmic::Theme, cosmic::Renderer>,
    handler: fn(keyboard::Key, keyboard::Modifiers) -> Option<Message>,
}

impl<'a, Message> KeyboardWrapper<'a, Message> {
    /// Creates a [`KeyboardWrapper`] with the given content.
    pub fn new(
        content: impl Into<Element<'a, Message, cosmic::Theme, cosmic::Renderer>>,
        handler: fn(keyboard::Key, keyboard::Modifiers) -> Option<Message>,
    ) -> Self {
        KeyboardWrapper {
            content: content.into(),
            handler,
        }
    }
}

impl<Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for KeyboardWrapper<'_, Message>
where
    Message: Clone,
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
    ) -> Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut state.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut state.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && let Some(message) = (self.handler)(key.clone(), *modifiers)
        {
            shell.publish(message);
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
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
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
        viewport: &cosmic::iced::Rectangle,
        translation: cosmic::iced::Vector,
    ) -> Option<overlay::Element<'b, Message, cosmic::Theme, cosmic::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }

    fn drag_destinations(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        dnd_rectangles: &mut cosmic::iced::core::clipboard::DndDestinationRectangles,
    ) {
        if let Some(child) = state.children.first() {
            self.content
                .as_widget()
                .drag_destinations(child, layout, renderer, dnd_rectangles);
        }
    }
}

impl<'a, Message> From<KeyboardWrapper<'a, Message>>
    for Element<'a, Message, cosmic::Theme, cosmic::Renderer>
where
    Message: 'a + Clone,
{
    fn from(
        area: KeyboardWrapper<'a, Message>,
    ) -> Element<'a, Message, cosmic::Theme, cosmic::Renderer> {
        Element::new(area)
    }
}
