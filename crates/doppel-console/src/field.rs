//! A one-line text field, for the few names and labels a dialog asks for:
//! typed characters go to its end, Backspace takes the last one away, ⌘V
//! pastes, Return submits and Escape cancels.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, Styled, Window, div, px,
};

use crate::theme::{SANS, Theme};

/// What the field tells its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldEvent {
    Edited,
    Submit,
    Cancel,
}

#[derive(Debug)]
pub(crate) struct TextField {
    text: String,
    focus: FocusHandle,
}

impl TextField {
    pub(crate) fn new(text: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self {
            text: text.into(),
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if modifiers.platform {
            if keystroke.key == "v"
                && let Some(pasted) = cx.read_from_clipboard().and_then(|item| item.text())
            {
                self.text.extend(pasted.chars().filter(|c| !c.is_control()));
                cx.emit(FieldEvent::Edited);
                cx.notify();
                cx.stop_propagation();
            }
            return;
        }
        match keystroke.key.as_str() {
            "backspace" => {
                if modifiers.alt {
                    self.text.clear();
                } else {
                    self.text.pop();
                }
                cx.emit(FieldEvent::Edited);
            }
            "enter" => cx.emit(FieldEvent::Submit),
            "escape" => cx.emit(FieldEvent::Cancel),
            _ => match &keystroke.key_char {
                Some(typed) if !modifiers.control && typed.chars().all(|c| !c.is_control()) => {
                    self.text.push_str(typed);
                    cx.emit(FieldEvent::Edited);
                }
                Some(_) | None => return,
            },
        }
        cx.notify();
        cx.stop_propagation();
    }
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let focused = self.focus.is_focused(window);
        let text: SharedString = self.text.clone().into();
        div()
            .id("field")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|field, event, _, cx| field.key(event, cx)))
            .on_click(cx.listener(|field, _, window, cx| window.focus(&field.focus, cx)))
            .flex()
            .items_center()
            .h(px(34.))
            .px(px(10.))
            .w_full()
            .border_1()
            .border_color(if focused {
                theme.accent
            } else {
                theme.line_strong
            })
            .when(focused, |field| {
                field.shadow(vec![Theme::ring(theme.accent_soft, 2.)])
            })
            .bg(theme.surface)
            .text_color(theme.text)
            .text_size(px(14.))
            .font_family(SANS)
            .cursor_text()
            .child(text)
            .when(focused, |field| {
                field.child(div().ml(px(1.)).w(px(1.5)).h(px(17.)).bg(theme.accent))
            })
    }
}

use gpui::StatefulInteractiveElement as _;
use gpui::prelude::FluentBuilder as _;
