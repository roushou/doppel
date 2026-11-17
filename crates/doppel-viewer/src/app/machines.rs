//! The machines page: the stage, each machine's panel (its card, screen,
//! rail and menu), and the launcher of an empty bay, drawn.

use std::sync::Arc;

use doppel_machine::machine::MachineName;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, deferred, div,
    prelude::FluentBuilder, px, relative, rgb,
};

use super::{
    FACTS, FACTS_NARROW, HEAD, Lens, LensKind, MenuItem, NARROW, RAIL, RIM, Root, Segment,
    WINDOW_MS, dot, end_cell, id, ink, segment,
};
use crate::ask::{Choice, Question};
use crate::elements::Named;
use crate::icons::Icon;
use crate::inspect::{self, Act, Empty, Looking, Pointing};
use crate::model::{
    Action, Group, Hold, Machine, MachineState, Provenance, Run, Session, SessionActivity, forks_of,
};
use crate::overlay;
use crate::panel::Overlay;
use crate::reading;
use crate::report::{self, Guest, Login};
use crate::steering::Steering;
use crate::theme::{Hue, MONO, Theme};
use crate::timeline;
use crate::ui::{self, Button, Enabled, Look};

/// The icon of an action's button.
const fn action_icon(action: Action) -> Icon {
    match action {
        Action::Start | Action::StartGolden | Action::Resume => Icon::Play,
        Action::FromTemplate => Icon::Plus,
        Action::Stop => Icon::Power,
        Action::ForceStop => Icon::CircleStop,
        Action::Pause => Icon::Pause,
        Action::Checkpoint => Icon::Bookmark,
        Action::RollBack => Icon::UndoTwo,
        Action::Restore => Icon::RotateCcw,
        Action::Fork => Icon::GitFork,
        Action::NewCopy => Icon::Copy,
        Action::Delete => Icon::TrashTwo,
    }
}

/// The take-control button's width: its longest label, "Taking control…",
/// in 11 px Geist Mono (0.6 em a glyph), with its icon, gap and padding.
const CONTROL_WIDTH: f32 = 15. * 6.6 + 13. + 6. + 2. * 16. + 2.;

/// A cell of a panel's footer, its words in small mono upper case.
fn cell(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .border_r_1()
        .border_color(theme.line)
}

/// "1 agent", "2 agents".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The machines page, drawn.
impl Root {
    fn facts(&self, machine: &Machine, narrow: bool, now: f64, theme: &Theme) -> Div {
        let mut left: Vec<Div> = Vec::new();
        let mut right: Vec<Div> = Vec::new();
        let live = machine.state.is_live();
        let report = self
            .panels
            .get(&machine.name)
            .and_then(|panel| panel.report());
        if let (true, Some(report)) = (live, report) {
            match &report.guest {
                Guest::Answering {
                    login,
                    front,
                    display,
                } => {
                    if !narrow {
                        left.push(cell(theme).child(match login {
                            Login::Locked => "SCREEN LOCKED",
                            Login::Active | Login::Unknown => "GUEST READY",
                        }));
                    }
                    if let Some(front) = front {
                        left.push(cell(theme).text_color(theme.text).child(front.clone()));
                    }
                    if let (Some(display), false) = (display, narrow) {
                        left.push(cell(theme).child(ui::upper(&display.words())));
                    }
                }
                Guest::Unreachable { .. } => left.push(
                    cell(theme)
                        .text_color(theme.warning)
                        .child("GUEST AGENT UNREACHABLE"),
                ),
                Guest::NotRunning | Guest::Unknown => {
                    left.push(cell(theme).child("GUEST AGENT NOT UP YET"));
                }
            }
        }
        if let (Some(report), false) = (report, narrow) {
            left.push(cell(theme).child(ui::upper(&count(
                report.checkpoints,
                "checkpoint",
                "checkpoints",
            ))));
        }
        if !narrow {
            match (machine.provenance(), machine.template()) {
                (Some(Provenance::Said(said)), None) => {
                    left.push(cell(theme).child(ui::upper(&said)));
                }
                (Some(Provenance::Unreadable), _) => left.push(
                    cell(theme)
                        .text_color(theme.warning)
                        .child("LINEAGE UNREADABLE"),
                ),
                (Some(Provenance::Said(_)), Some(_)) | (None, _) => {}
            }
        }
        if live {
            if self.drivers(&machine.name).next().is_none() {
                right.push(end_cell(theme).child("NO AGENTS"));
            }
            let starts = self
                .panels
                .get(&machine.name)
                .map_or(&[][..], |panel| panel.timeline())
                .iter()
                .map(|moment| moment.start_ms);
            right.push(
                end_cell(theme).child(format!("{} / MIN", report::calls_per_minute(starts, now))),
            );
        }
        div()
            .flex()
            .flex_none()
            .h(px(FACTS))
            .min_w_0()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .font_family(MONO)
            .text_size(px(10.5))
            .text_color(theme.muted)
            .whitespace_nowrap()
            .children(left)
            .child(div().flex_1())
            .children(right)
    }

    /// One agent's card in a panel's rail: who it is and what it's doing.
    fn card(
        &self,
        session: &Session,
        live: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hue = Hue::of(session.id.0);
        let paused = session.activity == SessionActivity::Paused;
        let held = session.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(14.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(hue.fill)
                            .text_color(rgb(0x0010_141b))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(session.initial().to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(ui::upper(session.who())),
                    )
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child(session.answered.to_string()),
                    )
                    .child(
                        div()
                            .id(ElementId::NamedInteger("hold".into(), session.id.0))
                            .flex_none()
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(theme.line)
                            .cursor_pointer()
                            .hover(|button| button.bg(theme.hover))
                            .child(
                                if paused { Icon::Play } else { Icon::Pause }.svg(12., theme.muted),
                            )
                            .on_click(cx.listener(move |root, _, _, cx| {
                                root.hold(
                                    &held,
                                    if paused { Hold::Resume } else { Hold::Pause },
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .mt(px(4.))
                    .font_family(MONO)
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted)
                    .child("NOW"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .truncate()
                    .text_color(if paused { theme.warning } else { theme.text })
                    .child(session.doing(live)),
            )
            .into_any_element()
    }

    /// A machine's panel, `width` wide: its head row, its screen with the
    /// rail beside it (under it, when narrow), and the tape of its calls.
    fn panel(
        &mut self,
        machine: &Machine,
        width: f32,
        frame_height: Option<f32>,
        now: f64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = machine.name.clone();
        let busy = self.panels.get(&name).and_then(|panel| panel.action());
        let shown = self
            .panels
            .get(&name)
            .and_then(|panel| panel.screen().open())
            .map(|shown| shown.view.clone());
        let driving = self.driving(&name, cx);
        let live = machine.state.is_live();
        let running = machine.state == MachineState::Live(Run::Running);
        let paused = machine.state == MachineState::Live(Run::Paused);
        let overlay_on = self
            .panels
            .get(&name)
            .is_none_or(|panel| panel.overlay() == Overlay::Shown);
        let allowed = machine.actions();
        let beside = width >= NARROW;
        let lens = self
            .panels
            .get(&name)
            .map_or(LensKind::Screen, |panel| panel.handles().lens.kind());
        let moments = self
            .panels
            .get(&name)
            .map(|panel| panel.timeline().to_vec())
            .unwrap_or_default();
        // What the elements tree shows: the agent's last snapshot, or the
        // person's own look, whichever is newer.
        let panel = ElementsShown::of(
            &moments,
            self.panels.get(&name).and_then(|panel| panel.look()),
            now,
        );
        let scene = timeline::scene_at(&moments, now);
        // The call each agent is on, as the activity feed shows it, and since when.
        let on_call = |session: &Session| {
            scene
                .of(session.id.0)
                .and_then(|agent| agent.current.as_ref())
                .map(|(call, since)| (call.describe(), *since))
        };
        if let Some(Lens::Elements(inspect)) = self
            .panels
            .get_mut(&name)
            .map(|kept| &mut kept.handles_mut().lens)
        {
            inspect.follow(panel.snapshot.as_deref().ok());
        }
        let act: Act = {
            let pointed = name.clone();
            let listener = cx.listener(move |root, pointing: &Pointing, _, cx| {
                root.inspected(&pointed, pointing.clone(), cx);
            });
            std::rc::Rc::new(move |pointing, window, cx| listener(&pointing, window, cx))
        };

        // The head row, in the same order in every panel: its name, its
        // role, its state, the lens, the overlay; then the actions, the
        // menu, and control.
        let (state_word, tone) = machine.state.look();
        let mut head = div()
            .relative()
            .flex()
            .flex_none()
            .h(px(HEAD))
            .min_w_0()
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .child(
                div()
                    .flex()
                    .flex_shrink(1.)
                    .min_w(px(60.))
                    .max_w(px(width * 0.3))
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .border_r_1()
                    .border_color(theme.line)
                    .font_family(MONO)
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(div().min_w_0().truncate().child(name.to_string()))
                    // A role, not a warning: neutral.
                    .when(machine.template().is_some(), |cell| {
                        cell.child(
                            div()
                                .flex_none()
                                .px(px(5.))
                                .py(px(2.))
                                .border_1()
                                .border_color(theme.line_strong)
                                .text_size(px(9.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted)
                                .child("TEMPLATE"),
                        )
                    }),
            );
        let state = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .border_r_1()
            .border_color(theme.line)
            .font_family(MONO)
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();
        head = match busy {
            Some(action) => head.child(
                state
                    .text_color(theme.accent)
                    .child(ui::spinner(id("panel-busy", &name), 11., theme.accent))
                    .child(ui::upper(action.under_way())),
            ),
            None => head.child(
                state
                    .text_color(ink(tone, theme))
                    .child(ui::dot(7., ink(tone, theme)))
                    .child(ui::upper(state_word)),
            ),
        };
        if live {
            head = head.child(
                div()
                    .flex()
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(theme.line)
                    .children(LensKind::ALL.into_iter().enumerate().map(|(index, kind)| {
                        let toggled = name.clone();
                        segment(
                            id(&format!("lens-{}", kind.label()), &name),
                            None,
                            Some(kind.label()),
                            if kind == lens {
                                Segment::On
                            } else {
                                Segment::Plain
                            },
                            theme,
                        )
                        .when(!beside, |segment| segment.px(px(8.)))
                        .when(index + 1 < LensKind::ALL.len(), |segment| {
                            segment.border_r_1().border_color(theme.line)
                        })
                        .on_click(cx.listener(move |root, _, _, cx| {
                            let same = root
                                .panels
                                .get(&toggled)
                                .map_or(LensKind::Screen, |panel| panel.handles().lens.kind())
                                == kind;
                            if !same {
                                root.panels.of(&toggled).handles_mut().lens = kind.lens();
                            }
                            cx.notify();
                        }))
                    })),
            );
            let toggled = name.clone();
            head = head.child(
                segment(
                    id("overlay", &name),
                    Some(if overlay_on { Icon::Eye } else { Icon::EyeOff }),
                    None,
                    if overlay_on {
                        Segment::Plain
                    } else {
                        Segment::Faint
                    },
                    theme,
                )
                .border_r_1()
                .border_color(theme.line)
                .on_click(cx.listener(move |root, _, _, cx| {
                    root.panels.of(&toggled).toggle_overlay();
                    cx.notify();
                })),
            );
        }
        head = head.child(div().flex_1().min_w_0());
        // Labelled actions. A machine with no screen shows its main action
        // in the middle of the panel instead, so it's offered once.
        let in_head = |action: &Action| {
            action.shown()
                && (live
                    || match action {
                        Action::Start | Action::FromTemplate => false,
                        Action::StartGolden
                        | Action::Stop
                        | Action::ForceStop
                        | Action::Pause
                        | Action::Resume
                        | Action::Checkpoint
                        | Action::RollBack
                        | Action::Restore
                        | Action::Fork
                        | Action::NewCopy
                        | Action::Delete => true,
                    })
        };
        for action in allowed.iter().copied().filter(in_head) {
            let picked = name.clone();
            let tool = segment(
                id(action.label(), &name),
                Some(action_icon(action)),
                beside.then(|| action.label()),
                Segment::Plain,
                theme,
            )
            .border_l_1()
            .border_color(theme.line);
            head =
                head.child(match busy {
                    None => tool.on_click(cx.listener(move |root, _, window, cx| {
                        root.pick(&picked, action, window, cx)
                    })),
                    Some(_) => tool.opacity(0.45).cursor_default(),
                });
        }
        let in_menu: Vec<MenuItem> = std::iter::once(MenuItem::Recordings)
            .chain(
                allowed
                    .iter()
                    .copied()
                    .filter(|action| !action.shown())
                    .map(MenuItem::Act),
            )
            .collect();
        head = head.child(self.menu_button(&name, in_menu, HEAD, theme, cx));
        if running {
            let steering = shown
                .as_ref()
                .and_then(|view| view.read(cx).steering().cloned());
            let label = steering.as_ref().map_or("Take control", Steering::button);
            let view = shown.clone();
            let control = segment(
                id("steer", &name),
                Some(if driving {
                    Icon::Hand
                } else {
                    Icon::MousePointer2
                }),
                Some(label),
                if driving {
                    Segment::SignalLine
                } else {
                    Segment::Signal
                },
                theme,
            )
            // One width for every label, so the head never shifts as
            // control changes hands; a border in both, for the same reason.
            .w(px(CONTROL_WIDTH))
            .border_1()
            .border_color(theme.signal);
            head = head.child(match view {
                Some(view) => control
                    .on_click(cx.listener(move |_, _, _, cx| {
                        view.update(cx, |view, cx| view.press(cx));
                    }))
                    .into_any_element(),
                None => control.opacity(0.45).cursor_default().into_any_element(),
            });
        }

        // The frame: the screen, or why there isn't one.
        let frame = div()
            .relative()
            .bg(theme.screen)
            .overflow_hidden()
            .map(|frame| match frame_height {
                Some(height) => frame.h(px(height)).flex_none(),
                None => frame.flex_1().min_h_0(),
            })
            .min_w_0()
            .when(beside, |frame| frame.flex_1());
        let empty = |icon: Icon, words: String| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(16.))
                .text_color(theme.faint)
                .child(icon.svg(28., theme.faint))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.muted)
                        .child(words),
                )
        };
        let frame = match &machine.state {
            MachineState::Live(_) => {
                // The screen and what's drawn over it sit under the rim,
                // with as much room under them, so the picture is centred.
                let outer = frame;
                let frame = div()
                    .absolute()
                    .top(px(RIM))
                    .left_0()
                    .right_0()
                    .bottom(px(RIM));
                let mut frame = match &shown {
                    Some(view) => frame.child(div().absolute().inset_0().child(view.clone())),
                    None => frame.child(
                        div()
                            .absolute()
                            .bottom(px(12.))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .px(px(12.))
                                    .py(px(5.))
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .bg(theme.overlay.opacity(0.85))
                                    .child(
                                        self.panels
                                            .get(&name)
                                            .and_then(|panel| panel.screen().failure())
                                            .map_or_else(|| "connecting…".into(), str::to_owned),
                                    ),
                            ),
                    ),
                };
                let picture = shown.as_ref().and_then(|view| view.read(cx).picture());
                if let (Some(inspect), Some(picture), Ok(snapshot)) =
                    (self.inspecting(&name, live), picture, &panel.snapshot)
                {
                    frame = frame.child(inspect::boxes(
                        snapshot,
                        &panel.named,
                        inspect,
                        picture,
                        panel.stale,
                        !driving,
                        &act,
                    ));
                }
                if let (LensKind::Text, Some(picture)) = (lens, picture) {
                    let read_now: reading::ReadNow = {
                        let asked = name.clone();
                        let listener = cx.listener(move |root, (): &(), _, cx| {
                            root.read_now(&asked, cx);
                        });
                        std::rc::Rc::new(move |window, cx| listener(&(), window, cx))
                    };
                    let text_look = self.panels.get(&name).and_then(|panel| panel.text_look());
                    let shown = timeline::reading(&moments, text_look, now);
                    let looking = match text_look.map(|look| &look.outcome) {
                        Some(timeline::TextLooked::Loading) => Looking::Busy,
                        Some(timeline::TextLooked::Read(_) | timeline::TextLooked::Failed(_))
                        | None => Looking::Ready,
                    };
                    let (drawn, whose, stale) = match &shown {
                        timeline::Reading::Agent(read) => (
                            reading::Drawn::Read(&read.read),
                            SharedString::from(format!("agent · {}", ago(now - read.since_ms))),
                            read.acted_since,
                        ),
                        timeline::Reading::Person { look, acted_since } => (
                            match &look.outcome {
                                timeline::TextLooked::Loading => reading::Drawn::Loading,
                                timeline::TextLooked::Read(read) => reading::Drawn::Read(read),
                                timeline::TextLooked::Failed(why) => reading::Drawn::Failed(why),
                            },
                            SharedString::from(format!("you · {}", ago(now - look.at_ms))),
                            *acted_since,
                        ),
                        timeline::Reading::Nothing => {
                            (reading::Drawn::Nothing, SharedString::default(), false)
                        }
                    };
                    frame = frame.child(reading::layer(
                        &format!("text-{name}"),
                        drawn,
                        picture,
                        whose,
                        stale,
                        Some(looking),
                        theme,
                        &read_now,
                    ));
                }
                if let (true, Some(picture)) = (overlay_on && !moments.is_empty(), picture) {
                    frame = frame.child(overlay::layer(&moments, picture, now));
                }
                if paused {
                    let resumed = name.clone();
                    frame = frame.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.screen.opacity(0.55))
                            .child(
                                Button::new(
                                    id("veil-resume", &name),
                                    theme,
                                    Look::Primary,
                                    Some(Icon::Play),
                                    Some("Resume".into()),
                                )
                                .enabled(Enabled::when(busy.is_none()))
                                .on_press(cx.listener(
                                    move |root, _, window, cx| {
                                        root.pick(&resumed, Action::Resume, window, cx);
                                    },
                                )),
                            ),
                    );
                }
                // Who has the machine: a rim round the screen and a tab
                // above it, orange for you, an agent's hue while it acts.
                // The agent whose call started last.
                let acting = self
                    .drivers(&name)
                    .filter_map(|driver| {
                        let live = on_call(driver);
                        let since = live.as_ref().map_or(f64::NEG_INFINITY, |(_, since)| *since);
                        driver
                            .working(live.as_ref().map(|(call, _)| call.as_str()))
                            .map(|call| {
                                (since, driver.id.0, driver.who().to_owned(), call.to_owned())
                            })
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, session, who, call)| (session, who, call));
                let (rim, tab) = if driving {
                    let since = self.panels.of(&name).controlled_since(now);
                    let seconds = ((now - since) / 1000.0).max(0.0) as u64;
                    (
                        Some(theme.signal),
                        Some((
                            theme.signal,
                            theme.on_signal,
                            Some(Icon::MousePointer2),
                            format!("YOU {:02}:{:02}", seconds / 60, seconds % 60),
                        )),
                    )
                } else {
                    self.panels.of(&name).released();
                    match &acting {
                        Some((session, who, call)) => {
                            let hue = Hue::of(*session);
                            (
                                Some(hue.fill),
                                Some((
                                    hue.fill,
                                    rgb(0x0010_141b).into(),
                                    None,
                                    format!("{} · {call}", ui::upper(who)),
                                )),
                            )
                        }
                        None => (None, None),
                    }
                };
                // The rim hugs the picture, not the bars beside it, and the
                // tab sits on its top left corner; the whole frame until the
                // picture's place is known.
                // The rim hugs the picture, not the bars beside it, and the
                // tab sits on its top left corner; the whole frame until the
                // picture's place is known.
                outer
                    .child(frame)
                    .children(rim.map(|rim| {
                        let ring = div().absolute().border_2().border_color(rim);
                        match picture {
                            Some(picture) => ring
                                .top(px(RIM + picture.top))
                                .left(px(picture.left))
                                .w(px(picture.width))
                                .h(px(picture.height)),
                            None => ring.top(px(RIM)).left_0().right_0().bottom(px(RIM)),
                        }
                    }))
                    .children(tab.map(|(fill, ink, icon, words)| {
                        div()
                            .absolute()
                            .top(px(picture.map_or(0., |picture| picture.top)))
                            .left(px(picture.map_or(0., |picture| picture.left)))
                            .h(px(RIM))
                            .max_w(
                                picture.map_or(gpui::Length::from(relative(0.8)), |picture| {
                                    gpui::Length::from(px(picture.width * 0.8))
                                }),
                            )
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(10.))
                            .bg(fill)
                            .text_color(ink)
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .children(icon.map(|icon| icon.svg(11., ink)))
                            .child(div().truncate().child(words))
                    }))
            }
            MachineState::Unreadable(why) => frame.child(empty(
                Icon::TriangleAlert,
                format!("Its status can't be read: {why}"),
            )),
            MachineState::Stopped => frame.child(self.stopped_card(machine, busy, theme, cx)),
        };

        // The rail: the agents driving it, and in the elements lens, the
        // elements tree under them.
        let drivers: Vec<Session> = self.drivers(&name).cloned().collect();
        let cards: Vec<AnyElement> = drivers
            .iter()
            .map(|driver| {
                let live = on_call(driver);
                self.card(
                    driver,
                    live.as_ref().map(|(call, _)| call.as_str()),
                    theme,
                    cx,
                )
            })
            .collect();
        let tree = self.inspecting(&name, live).map(|inspect| {
            div()
                .flex()
                .flex_1()
                .min_h(px(if frame_height.is_some() { 280. } else { 0. }))
                .child(inspect::tree(
                    &format!("tree-{name}"),
                    panel.snapshot.as_ref().map_err(Clone::clone),
                    &panel.named,
                    inspect,
                    panel.age.clone(),
                    panel.stale,
                    Some(panel.looking),
                    None,
                    theme,
                    &act,
                ))
        });
        let agents = div()
            .id(id("agents", &name))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .map(|agents| {
                if tree.is_some() {
                    agents.flex_none().max_h(relative(0.5))
                } else {
                    agents.flex_1().min_h_0()
                }
            })
            .map(|agents| {
                if cards.is_empty() {
                    agents.child(
                        div()
                            .px(px(14.))
                            .py(px(12.))
                            .text_size(px(12.))
                            .text_color(theme.muted)
                            .child("No agent has used this machine yet."),
                    )
                } else {
                    agents.children(cards)
                }
            });
        // The rail is there only with something in it: agents, or the tree.
        let railed = live && (!drivers.is_empty() || tree.is_some());
        let rail = railed.then(|| {
            div()
                .flex()
                .flex_col()
                .min_h_0()
                .overflow_hidden()
                .border_color(theme.line)
                .bg(theme.raised)
                .map(|rail| {
                    if beside {
                        rail.flex_none().w(px(RAIL)).border_l_1()
                    } else {
                        rail.border_t_1().when(frame_height.is_none(), |rail| {
                            rail.flex_none().max_h(relative(0.45))
                        })
                    }
                })
                .child(
                    div()
                        .flex_none()
                        .px(px(14.))
                        .py(px(9.))
                        .border_b_1()
                        .border_color(theme.line)
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .child(format!("AGENTS · {}", drivers.len())),
                )
                .when(!drivers.is_empty(), |rail| rail.child(agents))
                .children(tree)
        });
        let body = div()
            .flex()
            .min_h_0()
            .min_w_0()
            .map(|body| {
                if beside {
                    body.flex_row()
                } else {
                    body.flex_col()
                }
            })
            .map(|body| match frame_height {
                Some(_) => body.flex_none(),
                None => body.flex_1(),
            })
            .child(frame)
            .children(rail);

        // The tape: the last minute of calls. Always there for a live
        // machine, so the screen never jumps when the first call comes.
        let quiet = moments.is_empty();
        let tape = live.then(|| {
            div()
                .flex_none()
                .pt(px(6.))
                .px(px(12.))
                .pb(px(2.))
                .relative()
                .border_t_1()
                .border_color(theme.line)
                .when(quiet, |tape| {
                    tape.child(
                        div()
                            .absolute()
                            .left(px(12.))
                            .top(px(8.))
                            .font_family(MONO)
                            .text_size(px(10.5))
                            .text_color(theme.faint)
                            .child("NO CALLS YET"),
                    )
                })
                .child(overlay::strip(
                    id("timeline", &name),
                    &moments,
                    overlay::Span {
                        from: now - WINDOW_MS,
                        to: now,
                        now,
                        at: None,
                    },
                    move |ms| {
                        let seconds = ((ms - now) / 1000.0).round();
                        if seconds == 0.0 {
                            "now".to_owned()
                        } else {
                            format!("{seconds} s")
                        }
                    },
                    theme,
                ))
        });

        let facts = self.facts(machine, width < FACTS_NARROW, now, theme);
        div()
            .id(id("panel", &name))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.surface)
            .child(head)
            .child(body)
            .children(tape)
            .child(facts)
            .into_any_element()
    }

    /// What a stopped machine's panel shows instead of a screen: what it
    /// is, and its one main action. A golden image's makes a machine from
    /// it; starting it is in its menu, and asks first.
    fn stopped_card(
        &self,
        machine: &Machine,
        busy: Option<Action>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = machine.name.clone();
        let dim = theme.on_slab.opacity(0.6);
        let checkpoints = self
            .panels
            .get(&name)
            .and_then(|panel| panel.report())
            .map_or_else(|| "…".to_owned(), |report| report.checkpoints.to_string());
        let fact = |term: &str, value: AnyElement| {
            div()
                .flex()
                .gap(px(16.))
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(dim)
                        .child(ui::upper(term)),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let (title, said, action, second) = match machine.template() {
            Some(template) => {
                let forks = forks_of(&self.machines, &name);
                let made = if forks.is_empty() {
                    div().child("None yet").into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .children(forks.into_iter().map(|fork| {
                            let shown = fork.clone();
                            div()
                                .id(id("fork", &fork))
                                .underline()
                                .cursor_pointer()
                                .child(fork.to_string())
                                .on_click(cx.listener(move |root, _, _, cx| {
                                    if let Some(at) = root.stage.slot_of(&shown, &root.machines) {
                                        root.show_slot(at, cx);
                                    }
                                }))
                        }))
                        .into_any_element()
                };
                (
                    "TEMPLATE",
                    Some(format!(
                        "New machines start from its checkpoint {template}. Starting it lets it drift from that, so that's in the menu and asks first."
                    )),
                    Action::FromTemplate,
                    fact("Made from it", made),
                )
            }
            None => {
                let origin = match machine.provenance() {
                    Some(Provenance::Said(said)) => said,
                    Some(Provenance::Unreadable) => "Unreadable".to_owned(),
                    None => "Not recorded".to_owned(),
                };
                (
                    "STOPPED",
                    None,
                    Action::Start,
                    fact("Origin", div().child(origin).into_any_element()),
                )
            }
        };
        let started = name.clone();
        div().absolute().inset_0().flex().justify_center().child(
            div()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(14.))
                .w(px(420.))
                .max_w_full()
                .p(px(24.))
                .text_color(theme.on_slab)
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(dim)
                        .child(title),
                )
                .children(said.map(|said| div().text_size(px(13.)).child(said)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .child(fact(
                            "Checkpoints",
                            div().child(checkpoints).into_any_element(),
                        ))
                        .child(second),
                )
                .child(
                    div().flex().child(
                        Button::new(
                            id("empty-start", &name),
                            theme,
                            Look::Primary,
                            Some(action_icon(action)),
                            Some(action.label().into()),
                        )
                        .enabled(Enabled::when(busy.is_none()))
                        .on_press(cx.listener(
                            move |root, _, window, cx| {
                                root.pick(&started, action, window, cx);
                            },
                        )),
                    ),
                ),
        )
    }

    /// A machine's ⋯ button and, while open, its menu under it: `height`
    /// is the row it sits in.
    fn menu_button(
        &self,
        name: &MachineName,
        in_menu: Vec<MenuItem>,
        height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let toggled = name.clone();
        let mut menu = div().relative().flex().flex_none().h_full().child(
            segment(
                id("menu", name),
                Some(Icon::Ellipsis),
                None,
                Segment::Plain,
                theme,
            )
            .border_l_1()
            .border_color(theme.line)
            .on_click(cx.listener(move |root, _, _, cx| {
                // A press on the button that closed its menu leaves it closed.
                let just_closed = root
                    .menu_closed
                    .take()
                    .is_some_and(|(name, at)| name == toggled && root.now() - at < 500.0);
                root.menu = if just_closed || root.menu.as_ref() == Some(&toggled) {
                    None
                } else {
                    Some(toggled.clone())
                };
                cx.notify();
            })),
        );
        if self.menu.as_ref() == Some(name) {
            let items = in_menu.into_iter().map(|item| {
                let picked = name.clone();
                let (label, icon, danger) = match item {
                    MenuItem::Recordings => ("Recordings", Icon::Film, false),
                    MenuItem::Act(action) => (action.label(), action_icon(action), action.danger()),
                };
                let ink = if danger { theme.danger } else { theme.text };
                div()
                    .id(id(&format!("item-{label}"), name))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(theme.line)
                    .font_family(MONO)
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ink)
                    .cursor_pointer()
                    .hover(|item| item.bg(theme.hover))
                    .child(icon.svg(14., ink))
                    .child(ui::upper(label))
                    .on_click(cx.listener(move |root, _, window, cx| match item {
                        MenuItem::Recordings => root.recordings_of(Some(picked.clone()), cx),
                        MenuItem::Act(action) => root.pick(&picked, action, window, cx),
                    }))
            });
            menu = menu.child(deferred(
                div()
                    .id(id("menu-list", name))
                    .occlude()
                    .absolute()
                    .top(px(height - 1.))
                    .right_0()
                    .min_w(px(220.))
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(theme.line_strong)
                    .bg(theme.overlay)
                    .on_mouse_down_out(cx.listener(|root, _, _, cx| {
                        let now = root.now();
                        root.menu_closed = root.menu.take().map(|name| (name, now));
                        cx.notify();
                    }))
                    .children(items),
            ));
        }
        menu
    }

    /// What a free slot (`index`) can start: the machines not running,
    /// each with its main action (Start, or New machine from a template)
    /// and its menu, then New machine.
    pub(super) fn launcher(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        const ROW: f32 = 40.0;
        let heading = |text: &str, n: usize| {
            div()
                .flex()
                .gap(px(8.))
                .mt(px(16.))
                .mb(px(6.))
                .font_family(MONO)
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(ui::upper(text))
                .child(div().text_color(theme.faint).child(n.to_string()))
        };
        let mut list = div().w_full().flex().flex_col();
        for group in [Group::Stopped, Group::Golden] {
            let members: Vec<&Machine> = self
                .machines
                .iter()
                .filter(|machine| !machine.state.is_live() && machine.group() == group)
                .collect();
            if members.is_empty() {
                continue;
            }
            list = list.child(heading(group.title(), members.len()));
            let mut rows = div().border_t_1().border_color(theme.line);
            for machine in members {
                let name = machine.name.clone();
                let allowed = machine.actions();
                let main = [Action::FromTemplate, Action::Start]
                    .into_iter()
                    .find(|action| allowed.contains(action));
                let in_menu: Vec<MenuItem> = std::iter::once(MenuItem::Recordings)
                    .chain(
                        allowed
                            .iter()
                            .copied()
                            .filter(|action| Some(*action) != main)
                            .map(MenuItem::Act),
                    )
                    .collect();
                let busy = self.panels.get(&name).and_then(|panel| panel.action());
                let row = div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(ROW))
                    .pl(px(12.))
                    .border_b_1()
                    .border_color(theme.line)
                    .bg(theme.raised)
                    .child(match busy {
                        Some(_) => {
                            ui::spinner(id("bay-busy", &name), 10., theme.accent).into_any_element()
                        }
                        None => dot(machine.state.look().1, theme).into_any_element(),
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.strong)
                            .child(name.to_string()),
                    )
                    .children(machine.template().map(|template| {
                        div()
                            .px(px(5.))
                            .py(px(3.))
                            .border_1()
                            .border_color(theme.line_strong)
                            .font_family(MONO)
                            .text_size(px(10.))
                            .text_color(theme.muted)
                            .child(template.to_string())
                    }))
                    .child(div().flex_1())
                    .children(busy.map(|doing| {
                        div()
                            .pr(px(8.))
                            .font_family(MONO)
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .child(doing.under_way())
                    }))
                    .children(main.filter(|_| busy.is_none()).map(|action| {
                        let started = name.clone();
                        div()
                            .id(id("bay-go", &name))
                            .flex_none()
                            .size(px(ROW))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_l_1()
                            .border_color(theme.line)
                            .cursor_pointer()
                            .hover(|button| button.bg(theme.hover))
                            .child(action_icon(action).svg(13., theme.live))
                            .on_click(cx.listener(move |root, _, window, cx| {
                                if action == Action::Start {
                                    root.start_in(index, &started, window, cx);
                                } else {
                                    root.pick(&started, action, window, cx);
                                }
                            }))
                    }))
                    .child(self.menu_button(&name, in_menu, ROW, theme, cx));
                rows = rows.child(row);
            }
            list = list.child(rows);
        }
        list = list.child(
            div()
                .id(ElementId::NamedInteger("bay-new".into(), index as u64))
                .mt(px(14.))
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .border_1()
                .border_color(theme.line_strong)
                .font_family(MONO)
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .cursor_pointer()
                .hover(|button| button.bg(theme.hover))
                .child(Icon::Plus.svg(12., theme.text))
                .child("NEW MACHINE")
                .on_click(cx.listener(|root, _, window, cx| {
                    root.ask(
                        Question::Choose {
                            choice: Choice::NewCopy,
                            machine: None,
                            template: None,
                        },
                        window,
                        cx,
                    );
                })),
        );
        list
    }

    pub(super) fn stage_view(
        &mut self,
        width: f32,
        narrow: bool,
        now: f64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.machines.is_empty() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .p(px(24.))
                .text_color(theme.faint)
                .child(Icon::Monitor.svg(36., theme.faint))
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("No machines yet"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .text_color(theme.muted)
                        .child("Make one on the Mac with")
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .child("doppel create"),
                        )
                        .child("; it shows here as soon as it exists."),
                )
                .into_any_element();
        }
        // One machine fills the stage, or both side by side when split.
        self.stage.settle(&self.machines);
        let stage: Vec<Machine> = self
            .stage
            .shows(&self.machines)
            .into_iter()
            .cloned()
            .collect();
        if stage.is_empty() {
            // Nothing running: what can start, where the screen will be.
            return div()
                .id("idle")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .justify_center()
                .pt(relative(0.12))
                .child(div().w(px(420.)).child(self.launcher(0, theme, cx)))
                .into_any_element();
        }
        if narrow && stage.len() > 1 {
            // Stacked: each at the screen's shape.
            let frame_height = (width - 1.) * 10. / 16.;
            let panels: Vec<AnyElement> = stage
                .iter()
                .map(|machine| {
                    div()
                        .w_full()
                        .flex()
                        .border_b_1()
                        .border_color(theme.line)
                        .child(self.panel(machine, width, Some(frame_height), now, theme, cx))
                        .into_any_element()
                })
                .collect();
            return div()
                .id("stacked")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(panels)
                .into_any_element();
        }
        let each = width / stage.len() as f32;
        let panels: Vec<AnyElement> = stage
            .iter()
            .enumerate()
            .map(|(index, machine)| {
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(index > 0, |panel| {
                        panel.border_l_1().border_color(theme.line)
                    })
                    .child(self.panel(machine, each, None, now, theme, cx))
                    .into_any_element()
            })
            .collect();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .children(panels)
            .into_any_element()
    }
}

/// How long ago, in words.
fn ago(ms: f64) -> String {
    let seconds = (ms / 1000.0).max(0.0).round();
    if seconds < 60.0 {
        format!("{seconds} s ago")
    } else {
        format!("{} min ago", (seconds / 60.0).round())
    }
}

/// What a machine's elements panel shows, worked out from its timeline and
/// the person's latest look.
pub(super) struct ElementsShown {
    pub(super) snapshot: Result<Arc<doppel_machine::ax::tree::AxSnapshot>, Empty>,
    /// The agent's refs for it; none for a person's look.
    pub(super) named: Named,
    pub(super) age: SharedString,
    pub(super) stale: bool,
    pub(super) looking: Looking,
}

impl ElementsShown {
    pub(super) fn of(
        moments: &[timeline::Moment],
        look: Option<&timeline::Look>,
        now: f64,
    ) -> Self {
        let looking = match look.map(|look| &look.outcome) {
            Some(timeline::Looked::Loading) => Looking::Busy,
            Some(timeline::Looked::Seen(_) | timeline::Looked::Failed(_)) | None => Looking::Ready,
        };
        match timeline::showing(moments, look, now) {
            timeline::Showing::Agent(seen) => {
                let (snapshot, named) = match &seen.seen {
                    timeline::Seen::Live(snapshot) => (
                        Ok(snapshot.clone()),
                        Named::of(snapshot, seen.refs.as_deref()),
                    ),
                    timeline::Seen::Recorded(_) => (Err(Empty::None), Named::default()),
                };
                Self {
                    snapshot,
                    named,
                    age: format!("agent · {}", ago(now - seen.since_ms)).into(),
                    stale: seen.acted_since,
                    looking,
                }
            }
            timeline::Showing::Person { look, acted_since } => Self {
                snapshot: match &look.outcome {
                    timeline::Looked::Loading => Err(Empty::Loading),
                    timeline::Looked::Seen(snapshot) => Ok(snapshot.clone()),
                    timeline::Looked::Failed(why) => Err(Empty::Failed(why.clone())),
                },
                named: Named::default(),
                age: format!("you · {}", ago(now - look.at_ms)).into(),
                stale: acted_since,
                looking,
            },
            timeline::Showing::Nothing => Self {
                snapshot: Err(Empty::None),
                named: Named::default(),
                age: SharedString::default(),
                stale: false,
                looking,
            },
        }
    }
}
