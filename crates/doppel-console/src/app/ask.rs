//! What the window asks before an action runs: a checkpoint's label, which
//! checkpoint to go back to or start from, a fork's name, or that deleting
//! is meant. The web console's `Ask.svelte`.

use std::sync::Arc;

use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::machine::MachineName;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, div, prelude::FluentBuilder,
    px,
};

use crate::model::{Action, Machine, Order};
use crate::ui::field::{FieldEvent, TextField};
use crate::ui::icons::Icon;
use crate::ui::theme::{MONO, Theme};
use crate::ui::{self, Button, Enabled, Look};
use crate::{Daemon, LocalTime};

/// An action that needs a checkpoint chosen first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Choice {
    RollBack,
    Restore,
    Fork,
    NewCopy,
}

/// What the window asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Question {
    /// The label of a checkpoint to save.
    Checkpoint(MachineName),
    /// A checkpoint to use; with no machine, the machine first (a new one).
    /// `template` is chosen once read, when it's among them: a golden
    /// image's.
    Choose {
        choice: Choice,
        machine: Option<MachineName>,
        template: Option<CheckpointLabel>,
    },
    ForceStop(MachineName),
    /// That starting a golden image, which lets it drift from its
    /// template, is meant.
    StartGolden {
        machine: MachineName,
        template: CheckpointLabel,
    },
    /// That it's meant: the machine's name, typed.
    Delete(MachineName),
}

impl Question {
    /// What to ask before `action` on `machine`, if anything.
    pub(crate) fn before(action: Action, of: &Machine) -> Option<Self> {
        let machine = of.name.clone();
        let template = of.template().cloned();
        match action {
            Action::Start | Action::Stop | Action::Pause | Action::Resume => None,
            // No longer golden by the time it's picked: ask nothing more.
            Action::StartGolden => template.map(|template| Self::StartGolden { machine, template }),
            Action::FromTemplate => Some(Self::Choose {
                choice: Choice::NewCopy,
                machine: Some(machine),
                template,
            }),
            Action::ForceStop => Some(Self::ForceStop(machine)),
            Action::Delete => Some(Self::Delete(machine)),
            Action::Checkpoint => Some(Self::Checkpoint(machine)),
            Action::RollBack => Some(Self::choose(Choice::RollBack, machine)),
            Action::Restore => Some(Self::choose(Choice::Restore, machine)),
            Action::Fork => Some(Self::choose(Choice::Fork, machine)),
            Action::NewCopy => Some(Self::choose(Choice::NewCopy, machine)),
        }
    }

    const fn choose(choice: Choice, machine: MachineName) -> Self {
        Self::Choose {
            choice,
            machine: Some(machine),
            template: None,
        }
    }
}

/// What the dialog tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskEvent {
    Submit(MachineName, Order),
    Close,
}

/// A machine's checkpoints, as read so far.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Checkpoints {
    Reading,
    Read(Vec<CheckpointLabel>),
    Unreadable(String),
}

pub(crate) struct Ask {
    question: Question,
    daemon: Arc<dyn Daemon>,
    /// The machines a new one can start from.
    machines: Vec<MachineName>,
    /// The machine picked to start a new one from.
    source: Option<MachineName>,
    loaded: Checkpoints,
    chosen: Option<CheckpointLabel>,
    /// The checkpoint's label, or the name typed to confirm deleting.
    typed: Entity<TextField>,
    /// The fork's name: suggested from the machine and checkpoint until the
    /// person edits it.
    into: Entity<TextField>,
    into_edited: bool,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _fields: [Subscription; 2],
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask")
            .field("question", &self.question)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<AskEvent> for Ask {}

impl Focusable for Ask {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A label suggested for a checkpoint saved at `at`: `cp-20261115-0930`.
fn suggestion(at: LocalTime) -> String {
    format!(
        "cp-{:04}{:02}{:02}-{:02}{:02}",
        at.year, at.month, at.day, at.hour, at.minute
    )
}

/// The name suggested for a fork of `machine` at `label`.
fn fork_name(machine: &MachineName, label: &CheckpointLabel) -> String {
    let joined: String = format!("{machine}-{label}").replace('_', "-");
    let cut: String = joined.chars().take(MachineName::MAX_LEN).collect();
    cut.trim_end_matches('-').to_owned()
}

impl Ask {
    pub(crate) fn new(
        question: Question,
        daemon: Arc<dyn Daemon>,
        machines: Vec<MachineName>,
        now: LocalTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let typed_text = match &question {
            Question::Checkpoint(_) => suggestion(now),
            Question::Choose { .. }
            | Question::ForceStop(_)
            | Question::StartGolden { .. }
            | Question::Delete(_) => String::new(),
        };
        let typed = cx.new(|cx| TextField::new(typed_text, cx));
        let into = cx.new(|cx| TextField::new(String::new(), cx));
        let fields = [
            cx.subscribe(&typed, |ask, _, event, cx| ask.field(*event, false, cx)),
            cx.subscribe(&into, |ask, _, event, cx| ask.field(*event, true, cx)),
        ];
        let source = match &question {
            Question::Choose { machine: None, .. } => machines.first().cloned(),
            Question::Choose {
                machine: Some(_), ..
            }
            | Question::Checkpoint(_)
            | Question::ForceStop(_)
            | Question::StartGolden { .. }
            | Question::Delete(_) => None,
        };
        let focus = cx.focus_handle();
        match &question {
            Question::Checkpoint(_) | Question::Delete(_) => {
                let handle = typed.focus_handle(cx);
                window.focus(&handle, cx);
            }
            Question::Choose { .. } | Question::ForceStop(_) | Question::StartGolden { .. } => {
                window.focus(&focus, cx);
            }
        }
        let mut ask = Self {
            question,
            daemon,
            machines,
            source,
            loaded: Checkpoints::Reading,
            chosen: None,
            typed,
            into,
            into_edited: false,
            focus,
            _load: None,
            _fields: fields,
        };
        ask.load(cx);
        ask
    }

    /// The machine the question is about, once there is one.
    fn machine(&self) -> Option<&MachineName> {
        match &self.question {
            Question::Choose { machine: None, .. } => self.source.as_ref(),
            Question::Choose {
                machine: Some(machine),
                ..
            }
            | Question::Checkpoint(machine)
            | Question::ForceStop(machine)
            | Question::StartGolden { machine, .. }
            | Question::Delete(machine) => Some(machine),
        }
    }

    /// Reads the machine's checkpoints, for a question that chooses one.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Question::Choose { .. } = self.question else {
            return;
        };
        let Some(machine) = self.machine().cloned() else {
            return;
        };
        self.loaded = Checkpoints::Reading;
        self.chosen = None;
        let read = self.daemon.checkpoints(&machine);
        self._load = Some(cx.spawn(async move |ask, cx| {
            let read = read.await;
            let _ = ask.update(cx, |ask, cx| {
                if ask.machine() != Some(&machine) {
                    return;
                }
                match read {
                    Ok(labels) => {
                        ask.chosen = Self::preselected(&ask.question, &labels);
                        ask.loaded = Checkpoints::Read(labels);
                        ask.suggest(cx);
                    }
                    Err(error) => ask.loaded = Checkpoints::Unreadable(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    /// The checkpoint chosen once they're read: a golden image's template
    /// when it's among them, else the newest.
    fn preselected(question: &Question, labels: &[CheckpointLabel]) -> Option<CheckpointLabel> {
        let template = match question {
            Question::Choose {
                template: Some(template),
                ..
            } => labels.iter().find(|label| *label == template),
            Question::Choose { template: None, .. }
            | Question::Checkpoint(_)
            | Question::ForceStop(_)
            | Question::StartGolden { .. }
            | Question::Delete(_) => None,
        };
        template.or_else(|| labels.last()).cloned()
    }

    /// Suggests the fork's name, until the person edits it.
    fn suggest(&mut self, cx: &mut Context<Self>) {
        if self.into_edited {
            return;
        }
        let name = match (self.machine(), &self.chosen) {
            (Some(machine), Some(label)) => fork_name(machine, label),
            (None, _) | (_, None) => String::new(),
        };
        self.into.update(cx, |field, cx| field.set_text(name, cx));
    }

    fn field(&mut self, event: FieldEvent, fork_name: bool, cx: &mut Context<Self>) {
        match event {
            FieldEvent::Edited => {
                if fork_name {
                    self.into_edited = true;
                }
                cx.notify();
            }
            FieldEvent::Submit => self.submit(cx),
            FieldEvent::Cancel => cx.emit(AskEvent::Close),
        }
    }

    /// The order the form makes now; `None` while it isn't complete.
    fn order(&self, cx: &App) -> Option<(MachineName, Order)> {
        let typed = self.typed.read(cx).text().trim();
        match &self.question {
            Question::Checkpoint(machine) => typed
                .parse()
                .ok()
                .map(|label| (machine.clone(), Order::Checkpoint(label))),
            Question::ForceStop(machine) => Some((machine.clone(), Order::ForceStop)),
            Question::StartGolden { machine, .. } => Some((machine.clone(), Order::Start)),
            Question::Delete(machine) => {
                (typed == machine.as_str()).then(|| (machine.clone(), Order::Delete))
            }
            Question::Choose { choice, .. } => {
                let machine = self.machine()?.clone();
                let label = self.chosen.clone()?;
                let order = match choice {
                    Choice::RollBack => Order::RollBack(label),
                    Choice::Restore => Order::Restore(label),
                    Choice::NewCopy => Order::NewCopy(label),
                    Choice::Fork => Order::Fork {
                        label,
                        into: self.into.read(cx).text().trim().parse().ok()?,
                    },
                };
                Some((machine, order))
            }
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if let Some((machine, order)) = self.order(cx) {
            cx.emit(AskEvent::Submit(machine, order));
        }
    }

    fn title(&self) -> String {
        match &self.question {
            Question::Checkpoint(machine) => format!("Save a checkpoint of {machine}"),
            Question::ForceStop(machine) => format!("Force stop {machine}?"),
            Question::StartGolden { machine, .. } => format!("Start {machine}?"),
            Question::Delete(machine) => format!("Delete {machine}?"),
            Question::Choose {
                choice,
                machine,
                template,
            } => match (choice, machine) {
                (Choice::RollBack, Some(machine)) => format!("Roll {machine} back"),
                (Choice::Restore, Some(machine)) => format!("Restore {machine}"),
                (Choice::Fork, Some(machine)) => format!("Fork {machine}"),
                (Choice::NewCopy, Some(machine)) => match template {
                    Some(_) => format!("New machine from {machine}"),
                    None => format!("New copy of {machine}"),
                },
                (Choice::NewCopy | Choice::RollBack | Choice::Restore | Choice::Fork, None) => {
                    "New machine".into()
                }
            },
        }
    }

    const fn danger(&self) -> bool {
        match &self.question {
            Question::Delete(_) | Question::ForceStop(_) => true,
            Question::Choose { choice, .. } => match choice {
                Choice::RollBack | Choice::Restore => true,
                Choice::Fork | Choice::NewCopy => false,
            },
            Question::Checkpoint(_) | Question::StartGolden { .. } => false,
        }
    }

    const fn verb(&self) -> &'static str {
        match &self.question {
            Question::Checkpoint(_) => "Save",
            Question::ForceStop(_) => "Force stop",
            Question::StartGolden { .. } => "Start anyway",
            Question::Delete(_) => "Delete for good",
            Question::Choose { choice, .. } => match choice {
                Choice::RollBack => "Roll back",
                Choice::Restore => "Restore",
                Choice::Fork => "Fork",
                Choice::NewCopy => "Create and start",
            },
        }
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(AskEvent::Close),
            "enter" => self.submit(cx),
            _ => {}
        }
    }

    fn body(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let text = |words: String| div().child(words).into_any_element();
        let label = |words: &'static str| {
            div()
                .text_size(px(13.))
                .text_color(theme.muted)
                .child(words)
        };
        match &self.question {
            Question::Checkpoint(_) => {
                let typed = self.typed.read(cx).text().to_owned();
                let wrong = !typed.is_empty() && typed.trim().parse::<CheckpointLabel>().is_err();
                vec![
                    text("It saves the disk and memory now, then carries on. You can roll back to it, restore it, or start copies from it.".into()),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label("Label"))
                        .child(self.typed.clone())
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(if wrong { theme.danger } else { theme.faint })
                                .child("Lowercase letters, digits, - and _."),
                        )
                        .into_any_element(),
                ]
            }
            Question::ForceStop(machine) => vec![text(format!(
                "It powers {machine} off at once, as pulling the plug would. Anything unsaved in it is lost."
            ))],
            Question::StartGolden { machine, template } => vec![
                text(format!(
                    "{machine} is a golden image: new machines start from its checkpoint {template}."
                )),
                text(format!(
                    "Starting it lets it drift from that template: what changes in it isn't in {template}, and new machines won't have it. To work in a copy, make a new machine from it instead."
                )),
            ],
            Question::Delete(machine) => vec![
                div()
                    .flex()
                    .flex_wrap()
                    .child("This removes ")
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(machine.to_string()),
                    )
                    .child(", its disk and all its checkpoints, for good. Its recordings stay.")
                    .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .text_size(px(13.))
                            .text_color(theme.muted)
                            .child("Type")
                            .child(
                                div()
                                    .font_family(MONO)
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(machine.to_string()),
                            )
                            .child("to confirm"),
                    )
                    .child(self.typed.clone())
                    .into_any_element(),
            ],
            Question::Choose {
                choice,
                machine,
                template,
            } => {
                let mut body = Vec::new();
                match (machine, choice) {
                    (None, _) => {
                        body.push(text("A new machine starts from a checkpoint of one you have: its disk and login, under a new identity.".into()));
                        body.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .child(label("From"))
                                .child(self.sources(theme, cx))
                                .into_any_element(),
                        );
                    }
                    (Some(machine), Choice::RollBack) => body.push(text(format!(
                        "{machine} goes back to the checkpoint while it runs. Everything since is thrown away."
                    ))),
                    (Some(machine), Choice::Restore) => body.push(text(format!(
                        "{machine}'s disk and memory go back to the checkpoint. Everything since is thrown away."
                    ))),
                    (Some(machine), Choice::Fork) => body.push(text(format!(
                        "A new stopped machine with the checkpoint's disk and memory. It shares {machine}'s identity, so the two can't run at once."
                    ))),
                    (Some(machine), Choice::NewCopy) => body.push(text(match template {
                        Some(_) => format!(
                            "A new machine with the checkpoint's disk and login under a new identity, started. {machine} stays as it is."
                        ),
                        None => format!(
                            "A new machine with the checkpoint's disk and login under a new identity, started beside {machine}."
                        ),
                    })),
                }
                body.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .pb(px(6.))
                                .text_size(px(13.))
                                .text_color(theme.text)
                                .child("Checkpoint"),
                        )
                        .child(self.labels(theme, cx))
                        .into_any_element(),
                );
                if *choice == Choice::Fork {
                    body.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(label("New machine's name"))
                            .child(self.into.clone())
                            .into_any_element(),
                    );
                }
                body
            }
        }
    }

    /// The machines a new one can start from, to pick one.
    fn sources(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .children(self.machines.iter().enumerate().map(|(index, name)| {
                let on = self.source.as_ref() == Some(name);
                let picked = name.clone();
                choice_row(("source", index), on, theme)
                    .child(Icon::Monitor.svg(14., theme.text))
                    .child(name.to_string())
                    .on_click(cx.listener(move |ask, _, _, cx| {
                        ask.source = Some(picked.clone());
                        ask.into_edited = false;
                        ask.load(cx);
                        cx.notify();
                    }))
            }))
    }

    /// The machine's checkpoints, to choose one.
    fn labels(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let quiet = |words: String| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(theme.faint)
                .child(words)
        };
        match &self.loaded {
            Checkpoints::Reading => quiet("Reading checkpoints…".into())
                .child(ui::spinner("checkpoints-loading", 14., theme.faint))
                .into_any_element(),
            Checkpoints::Unreadable(reason) => div()
                .text_color(theme.danger)
                .child(reason.clone())
                .into_any_element(),
            Checkpoints::Read(labels) if labels.is_empty() => quiet(format!(
                "{} has no checkpoints yet.",
                self.machine().map(ToString::to_string).unwrap_or_default()
            ))
            .into_any_element(),
            Checkpoints::Read(labels) => div()
                .id("labels")
                .flex()
                .flex_col()
                .gap(px(4.))
                .max_h(px(220.))
                .overflow_y_scroll()
                .children(labels.iter().enumerate().map(|(index, label)| {
                    let on = self.chosen.as_ref() == Some(label);
                    let picked = label.clone();
                    choice_row(("label", index), on, theme)
                        .font_family(MONO)
                        .child(Icon::Bookmark.svg(14., theme.text))
                        .child(label.to_string())
                        .on_click(cx.listener(move |ask, _, _, cx| {
                            ask.chosen = Some(picked.clone());
                            ask.suggest(cx);
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        }
    }
}

/// One choice in a list: a checkpoint, or a machine.
fn choice_row(
    id: impl Into<gpui::ElementId>,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .py(px(8.))
        .border_1()
        .border_color(if on { theme.accent_fill } else { theme.line })
        .when(on, |row| row.bg(theme.accent_soft))
        .text_color(theme.text)
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|row| row.bg(theme.hover))
}

impl Render for Ask {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let ready = Enabled::when(self.order(cx).is_some());
        let title: SharedString = self.title().into();
        let body = self.body(&theme, cx);
        div()
            .id("ask")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|ask, event, _, cx| ask.key(event, cx)))
            .w(px(440.))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .pt(px(18.))
            .px(px(18.))
            .pb(px(16.))
            .border_1()
            .border_color(theme.line_strong)
            .bg(theme.overlay)
            .text_color(theme.muted)
            .text_size(px(14.))
            // Presses inside don't reach the backdrop, which closes it.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        Button::new("ask-close", &theme, Look::Ghost, Some(Icon::X), None)
                            .on_press(cx.listener(|_, _, _, cx| cx.emit(AskEvent::Close))),
                    ),
            )
            .child(div().flex().flex_col().gap(px(14.)).children(body))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new(
                            "ask-cancel",
                            &theme,
                            Look::Ghost,
                            None,
                            Some("Cancel".into()),
                        )
                        .on_press(cx.listener(|_, _, _, cx| cx.emit(AskEvent::Close))),
                    )
                    .child(
                        Button::new(
                            "ask-submit",
                            &theme,
                            if self.danger() {
                                Look::Danger
                            } else {
                                Look::Primary
                            },
                            None,
                            Some(self.verb().into()),
                        )
                        .enabled(ready)
                        .on_press(cx.listener(|ask, _, _, cx| ask.submit(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_label_is_suggested_from_the_time() {
        let at = LocalTime {
            year: 2026,
            month: 11,
            day: 5,
            hour: 9,
            minute: 7,
            second: 0,
        };
        assert_eq!(suggestion(at), "cp-20261105-0907");
        assert!(suggestion(at).parse::<CheckpointLabel>().is_ok());
    }

    #[test]
    fn a_golden_image_preselects_its_template_once_read() {
        let labels: Vec<CheckpointLabel> = ["ready", "later"]
            .iter()
            .map(|label| label.parse().unwrap())
            .collect();
        let choose = |template: Option<&str>| Question::Choose {
            choice: Choice::NewCopy,
            machine: Some("golden-27".parse().unwrap()),
            template: template.map(|label| label.parse().unwrap()),
        };
        assert_eq!(
            Ask::preselected(&choose(Some("ready")), &labels),
            Some(labels[0].clone())
        );
        // Gone, or not golden: the newest.
        assert_eq!(
            Ask::preselected(&choose(Some("gone")), &labels),
            Some(labels[1].clone())
        );
        assert_eq!(
            Ask::preselected(&choose(None), &labels),
            Some(labels[1].clone())
        );
        assert_eq!(Ask::preselected(&choose(None), &[]), None);
    }

    #[test]
    fn a_golden_image_asks_before_it_starts_and_makes_machines_from_its_template() {
        use doppel_machine::lineage::{Lineage, RecordedLineage, Role};
        let golden = Machine {
            name: "golden-27".parse().unwrap(),
            state: crate::model::MachineState::Stopped,
            lineage: RecordedLineage::Known(Lineage::default().with_role(Role::Golden {
                template: "ready".parse().unwrap(),
                agent: doppel_machine::lineage::TemplateAgent::Unrecorded,
            })),
        };
        assert_eq!(
            Question::before(Action::StartGolden, &golden),
            Some(Question::StartGolden {
                machine: golden.name.clone(),
                template: "ready".parse().unwrap(),
            })
        );
        assert_eq!(
            Question::before(Action::FromTemplate, &golden),
            Some(Question::Choose {
                choice: Choice::NewCopy,
                machine: Some(golden.name.clone()),
                template: Some("ready".parse().unwrap()),
            })
        );
        assert_eq!(Question::before(Action::Start, &golden), None);
    }

    #[test]
    fn a_fork_is_named_for_its_machine_and_checkpoint() {
        let machine: MachineName = "dev".parse().unwrap();
        let label: CheckpointLabel = "clean_install".parse().unwrap();
        assert_eq!(fork_name(&machine, &label), "dev-clean-install");
        let long: CheckpointLabel = "a-very-long-checkpoint-label-x".parse().unwrap();
        let named = fork_name(&machine, &long);
        assert!(named.len() <= MachineName::MAX_LEN && !named.ends_with('-'));
        assert!(named.parse::<MachineName>().is_ok());
    }
}
