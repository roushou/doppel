//! The recordings page: the recorded sessions, newest first, of one
//! machine first when it was opened from its panel.

use std::sync::Arc;

use doppel_machine::machine::MachineName;
use gpui::{
    AnyElement, Context, Div, ElementId, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div,
    prelude::FluentBuilder, px, relative,
};

use crate::icons::Icon;
use crate::recordings::{self, RecordingId, RecordingSummary};
use crate::theme::{Hue, MONO, Theme};
use crate::ui::{Button, Look};
use crate::{Daemon, LocalClock};

/// Where the list is.
enum Showing {
    Loading,
    Listed(Vec<RecordingSummary>),
    Failed(String),
}

/// What the list tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordingsEvent {
    /// The person picked a recording to replay.
    Open(RecordingId),
}

/// The recordings page.
pub(crate) struct Recordings {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    /// The machine whose recordings are listed; `None` for all.
    only: Option<MachineName>,
    showing: Showing,
    _load: Option<Task<()>>,
}

impl Recordings {
    /// Lists the recordings, of `only` first when it names a machine.
    pub(crate) fn new(
        daemon: Arc<dyn Daemon>,
        clock: Arc<dyn LocalClock>,
        only: Option<MachineName>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut recordings = Self {
            daemon,
            clock,
            only,
            showing: Showing::Loading,
            _load: None,
        };
        recordings.load(cx);
        recordings
    }

    /// The machine it was opened for.
    pub(crate) fn opened_for(&self) -> Option<&MachineName> {
        self.only.as_ref()
    }

    /// Lists the recordings again.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.showing = Showing::Loading;
        let listed = self.daemon.recordings();
        self._load = Some(cx.spawn(async move |page, cx| {
            let listed = listed.await;
            let _ = page.update(cx, |page, cx| {
                page.showing = match listed {
                    Ok(listed) => Showing::Listed(listed),
                    Err(error) => Showing::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn list(&self, listed: &[RecordingSummary], theme: &Theme, cx: &mut Context<Self>) -> Div {
        if listed.is_empty() {
            return div()
                .flex()
                .flex_wrap()
                .gap(px(4.))
                .text_color(theme.muted)
                .child("No recordings. An agent's session is recorded with")
                .child(code(theme, "doppel mcp --record"));
        }
        let machines = RecordingSummary::machines_of(listed);
        let shown: Vec<&RecordingSummary> = listed
            .iter()
            .filter(|summary| summary.shows(self.only.as_ref()))
            .collect();
        let filter = (machines.len() > 1).then(|| {
            let all = chip("filter-all", "All".into(), self.only.is_none(), theme).on_click(
                cx.listener(|page, _, _, cx| {
                    page.only = None;
                    cx.notify();
                }),
            );
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.))
                .mb(px(12.))
                .child(all)
                .children(machines.into_iter().map(|machine| {
                    let on = self.only.as_ref() == Some(&machine);
                    let label = SharedString::from(machine.as_str().to_owned());
                    chip(
                        ElementId::Name(format!("filter-{machine}").into()),
                        label,
                        on,
                        theme,
                    )
                    .on_click(cx.listener(move |page, _, _, cx| {
                        page.only = Some(machine.clone());
                        cx.notify();
                    }))
                }))
        });
        let longest = shown
            .iter()
            .map(|summary| summary.traced_ms)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let mono = |text: String, color| {
            div()
                .flex_none()
                .font_family(MONO)
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(text)
        };
        let rows: Vec<AnyElement> = if shown.is_empty() {
            vec![
                div()
                    .text_color(theme.muted)
                    .child(format!(
                        "No recordings of {}.",
                        self.only.as_ref().map_or("", MachineName::as_str)
                    ))
                    .into_any_element(),
            ]
        } else {
            recordings::by_day(shown, self.clock.local())
                .into_iter()
                .map(|group| {
                    let heading = div()
                        .flex()
                        .gap(px(8.))
                        .mt(px(14.))
                        .mb(px(6.))
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .child(group.day.name().to_uppercase())
                        .child(
                            div()
                                .text_color(theme.faint)
                                .child(group.recordings.len().to_string()),
                        );
                    let rows =
                        group.recordings.into_iter().map(|summary| {
                            let id = summary.id.clone();
                            let share = summary.traced_ms as f32 / longest;
                            div()
                                .id(ElementId::Name(format!("recording-{}", summary.id).into()))
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .h(px(36.))
                                .px(px(12.))
                                .border_b_1()
                                .border_color(theme.line)
                                .text_size(px(13.))
                                .cursor_pointer()
                                .hover(|row| row.bg(theme.raised))
                                .child(mono(summary.started.time_of_day(), theme.strong).w(px(44.)))
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(10.))
                                        .bg(Hue::of(summary.session).fill),
                                )
                                .child(
                                    div()
                                        .w(px(160.))
                                        .flex_none()
                                        .truncate()
                                        .text_color(theme.text)
                                        .child(if summary.machines.is_empty() {
                                            "—".to_owned()
                                        } else {
                                            summary
                                                .machines
                                                .iter()
                                                .map(MachineName::as_str)
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        }),
                                )
                                .child(
                                    div().flex_1().h(px(4.)).bg(theme.line).child(
                                        div()
                                            .h_full()
                                            .min_w(px(2.))
                                            .w(relative(share.clamp(0.0, 1.0)))
                                            .bg(theme.muted),
                                    ),
                                )
                                .child(
                                    mono(recordings::length(summary.traced_ms), theme.muted)
                                        .w(px(64.))
                                        .flex()
                                        .justify_end(),
                                )
                                .child(
                                    mono(summary.calls.to_string(), theme.muted)
                                        .w(px(64.))
                                        .flex()
                                        .justify_end()
                                        .gap(px(4.))
                                        .child(div().text_color(theme.faint).child(
                                            if summary.calls == 1 { "call" } else { "calls" },
                                        )),
                                )
                                .child(Icon::Film.svg(
                                    14.,
                                    if summary.videos == 0 {
                                        theme.line_strong
                                    } else {
                                        theme.muted
                                    },
                                ))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(RecordingsEvent::Open(id.clone()))
                                }))
                        });
                    div()
                        .flex()
                        .flex_col()
                        .child(heading)
                        .child(div().border_t_1().border_color(theme.line).children(rows))
                        .into_any_element()
                })
                .collect()
        };
        div()
            .children(filter)
            .child(div().flex().flex_col().children(rows))
    }
}

impl EventEmitter<RecordingsEvent> for Recordings {}

impl Render for Recordings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let body = match &self.showing {
            Showing::Loading => div().text_color(theme.muted).child("Loading recordings…"),
            Showing::Failed(why) => div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(10.))
                .child(notice(&theme).child(format!("Recordings: {why}")))
                .child(
                    Button::new(
                        "recordings-again",
                        &theme,
                        Look::Plain,
                        None,
                        Some("Try again".into()),
                    )
                    .on_press(cx.listener(|page, _, _, cx| page.load(cx))),
                ),
            Showing::Listed(listed) => {
                let listed = listed.clone();
                self.list(&listed, &theme, cx)
            }
        };
        div()
            .id("recordings-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.))
            .text_size(px(14.))
            .text_color(theme.text)
            .child(body)
    }
}

/// A filter chip: a pill that's filled when on.
fn chip(
    id: impl Into<ElementId>,
    label: SharedString,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(11.))
        .border_1()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|chip| {
            if on {
                chip.bg(theme.accent_soft)
                    .border_color(gpui::transparent_black())
                    .text_color(theme.text)
            } else {
                chip.border_color(theme.line_strong)
                    .text_color(theme.muted)
                    .hover(|chip| chip.bg(theme.hover).text_color(theme.text))
            }
        })
        .child(label)
}

pub(super) fn notice(theme: &Theme) -> Div {
    div()
        .px(px(12.))
        .py(px(8.))
        .bg(theme.warning_soft)
        .text_color(theme.text)
}

fn code(theme: &Theme, text: &'static str) -> Div {
    div()
        .font_family(MONO)
        .text_size(px(12.5))
        .text_color(theme.text)
        .child(text)
}
