use super::*;

fn dev() -> MachineName {
    "dev".parse().unwrap()
}

fn work() -> MachineName {
    "work".parse().unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "an arbitrary anchor: only differences from it are asserted"
)]
fn anchor() -> Instant {
    Instant::now()
}

const fn cleared(held_up: HeldUp) -> Cleared {
    Cleared {
        note: None,
        held_up,
    }
}

/// A scheduler with one connection.
fn scheduler() -> (Scheduler, ConnectionId) {
    let mut scheduler = Scheduler::default();
    let connection = scheduler.connection();
    (scheduler, connection)
}

#[test]
fn a_session_shows_its_machines_and_what_runs_until_it_closes() {
    let (mut scheduler, connection) = scheduler();
    let id = scheduler.open("doppel mcp".into());
    let other = scheduler.open("viewer".into());
    let calls: Vec<CallId> = [
        (Some(dev()), "snapshot"),
        (Some(dev()), "wait_for"),
        (None, "sessions"),
    ]
    .into_iter()
    .map(|(machine, caption)| {
        let call = scheduler.arrive(connection, None);
        scheduler.join(call, id, machine, caption.into());
        call
    })
    .collect();
    let _ = scheduler.on(Event::Finish(calls[0]));
    let _ = scheduler.on(Event::Finish(calls[2]));
    let listed = scheduler.list();
    assert_eq!(
        listed[0],
        SessionView {
            id,
            client: "doppel mcp".into(),
            machines: vec![dev()],
            answered: 2,
            running: vec!["wait_for".into()],
            state: SessionState::Active,
        }
    );
    assert_eq!(listed[1].id, other);
    assert_eq!(
        scheduler.close(id, SessionEnd::Disconnected),
        [Action::Finish(id)]
    );
    assert_eq!(scheduler.list().len(), 1);
    // A call ending after its session is ignored.
    assert_eq!(scheduler.on(Event::Finish(calls[1])), []);
}

#[test]
fn calls_wait_while_a_person_has_the_machine_then_get_a_note() {
    let (mut scheduler, connection) = scheduler();
    let agent = scheduler.open("doppel mcp".into());
    let viewer = scheduler.connection();
    let other_viewer = scheduler.connection();
    let t0 = anchor();
    let call = |scheduler: &mut Scheduler, machine: MachineName| {
        let call = scheduler.arrive(connection, None);
        scheduler.join(call, agent, Some(machine), String::new());
        call
    };
    let first = call(&mut scheduler, dev());
    let _ = scheduler.on(Event::Finish(first));
    assert_eq!(
        scheduler.control(&dev(), ControlEvent::Take { by: viewer, at: t0 }),
        Ok(Vec::new())
    );
    assert!(scheduler.holds(viewer, &dev()));
    assert!(!scheduler.holds(other_viewer, &dev()));
    assert!(
        scheduler
            .control(
                &dev(),
                ControlEvent::Take {
                    by: other_viewer,
                    at: t0
                }
            )
            .is_err()
    );
    // Another machine isn't held.
    let elsewhere = call(&mut scheduler, work());
    assert_eq!(
        scheduler.on(Event::Act(elsewhere)),
        [Action::Clear(elsewhere, cleared(HeldUp::No))]
    );
    // A call on the held machine waits until cancelled...
    let cancelled = call(&mut scheduler, dev());
    assert_eq!(scheduler.on(Event::Act(cancelled)), []);
    assert_eq!(
        scheduler.on(Event::Cancel(cancelled)),
        [Action::Refuse(cancelled)]
    );
    // ...or until the machine is handed back, with the note, once.
    let waiting = call(&mut scheduler, dev());
    assert_eq!(scheduler.on(Event::Act(waiting)), []);
    let back = t0 + Duration::from_secs(7);
    assert_eq!(
        scheduler.control(
            &dev(),
            ControlEvent::HandBack {
                by: viewer,
                at: back
            }
        ),
        Ok(vec![
            Action::PersonHeld {
                session: agent,
                machine: dev(),
                from: t0,
                to: back,
            },
            Action::Clear(
                waiting,
                Cleared {
                    note: Some(HandedBack {
                        machine: dev(),
                        held: Duration::from_secs(7),
                    }),
                    held_up: HeldUp::Yes,
                }
            ),
        ])
    );
    let after = call(&mut scheduler, dev());
    assert_eq!(
        scheduler.on(Event::Act(after)),
        [Action::Clear(after, cleared(HeldUp::No))]
    );
    // A viewer that leaves hands back what it held.
    let _ = scheduler.control(&dev(), ControlEvent::Take { by: viewer, at: t0 });
    let _ = scheduler.left(viewer, t0);
    assert!(!scheduler.holds(viewer, &dev()));
}

#[test]
fn a_paused_session_s_calls_wait_until_it_resumes() {
    let (mut scheduler, connection) = scheduler();
    let agent = scheduler.open("doppel mcp".into());
    assert_eq!(
        scheduler.session(agent.get(), SessionEvent::Pause),
        Ok(Vec::new())
    );
    assert_eq!(
        scheduler.session(agent.get(), SessionEvent::Pause),
        Err(PauseError::Session(SessionError::AlreadyPaused))
    );
    assert_eq!(scheduler.list()[0].state, SessionState::Paused);
    let call = scheduler.arrive(connection, None);
    scheduler.join(call, agent, None, String::new());
    assert_eq!(scheduler.on(Event::Act(call)), []);
    assert_eq!(
        scheduler.session(agent.get(), SessionEvent::Resume),
        Ok(vec![Action::Clear(call, cleared(HeldUp::Yes))])
    );
    assert_eq!(
        scheduler.session(agent.get(), SessionEvent::Resume),
        Err(PauseError::Session(SessionError::NotPaused))
    );
    assert_eq!(
        scheduler.session(99, SessionEvent::Pause),
        Err(PauseError::NoSuchSession(99))
    );
}

#[test]
fn closing_a_paused_session_lets_its_waiting_calls_go() {
    let (mut scheduler, connection) = scheduler();
    let agent = scheduler.open("doppel mcp".into());
    let _ = scheduler.session(agent.get(), SessionEvent::Pause);
    let call = scheduler.arrive(connection, None);
    scheduler.join(call, agent, None, String::new());
    assert_eq!(scheduler.on(Event::Act(call)), []);
    assert_eq!(
        scheduler.close(agent, SessionEnd::Disconnected),
        [
            Action::Clear(call, cleared(HeldUp::Yes)),
            Action::Finish(agent)
        ]
    );
}

#[test]
fn a_machine_serves_one_call_at_a_time_in_order() {
    let mut scheduler = Scheduler::default();
    let (one, two) = (scheduler.connection(), scheduler.connection());
    let first = scheduler.arrive(one, None);
    let second = scheduler.arrive(two, None);
    let third = scheduler.arrive(one, None);
    assert_eq!(
        scheduler.on(Event::Turn(first, dev())),
        [Action::Grant(first, dev())]
    );
    assert_eq!(scheduler.on(Event::Turn(second, dev())), []);
    assert_eq!(scheduler.on(Event::Turn(third, dev())), []);
    // Another machine isn't held up.
    let elsewhere = scheduler.arrive(two, None);
    assert_eq!(
        scheduler.on(Event::Turn(elsewhere, work())),
        [Action::Grant(elsewhere, work())]
    );
    assert_eq!(
        scheduler.on(Event::EndTurn(first, dev())),
        [Action::Grant(second, dev())]
    );
    // A call that ends holding the turn hands it on.
    assert_eq!(
        scheduler.on(Event::Finish(second)),
        [Action::Grant(third, dev())]
    );
}

#[test]
fn a_cancelled_call_gives_up_its_place_and_is_refused_from_then_on() {
    let (mut scheduler, connection) = scheduler();
    let (first, second, third) = (
        scheduler.arrive(connection, None),
        scheduler.arrive(connection, None),
        scheduler.arrive(connection, None),
    );
    let _ = scheduler.on(Event::Turn(first, dev()));
    let _ = scheduler.on(Event::Turn(second, dev()));
    let _ = scheduler.on(Event::Turn(third, dev()));
    assert_eq!(
        scheduler.on(Event::Cancel(second)),
        [Action::Refuse(second)]
    );
    assert_eq!(
        scheduler.on(Event::Turn(second, dev())),
        [Action::Refuse(second)]
    );
    assert_eq!(scheduler.on(Event::Act(second)), [Action::Refuse(second)]);
    assert_eq!(
        scheduler.on(Event::EndTurn(first, dev())),
        [Action::Grant(third, dev())]
    );
    // Cancelling one that holds the turn leaves it to finish.
    assert_eq!(scheduler.on(Event::Cancel(third)), []);
    let fourth = scheduler.arrive(connection, None);
    assert_eq!(scheduler.on(Event::Turn(fourth, dev())), []);
    assert_eq!(
        scheduler.on(Event::Finish(third)),
        [Action::Grant(fourth, dev())]
    );
}

/// The later call asks first, yet goes second.
#[test]
fn a_connection_s_calls_take_their_turns_in_the_order_they_arrived() {
    let mut scheduler = Scheduler::default();
    let (one, two) = (scheduler.connection(), scheduler.connection());
    let first = scheduler.arrive(one, Some(dev()));
    let second = scheduler.arrive(one, Some(dev()));
    let elsewhere = scheduler.arrive(two, Some(dev()));
    assert_eq!(scheduler.on(Event::Turn(second, dev())), []);
    // Another connection's call isn't held behind them.
    assert_eq!(
        scheduler.on(Event::Turn(elsewhere, dev())),
        [Action::Grant(elsewhere, dev())]
    );
    assert_eq!(scheduler.on(Event::Turn(first, dev())), []);
    assert_eq!(
        scheduler.on(Event::EndTurn(elsewhere, dev())),
        [Action::Grant(first, dev())]
    );
    assert_eq!(
        scheduler.on(Event::EndTurn(first, dev())),
        [Action::Grant(second, dev())]
    );
}

#[test]
fn a_call_that_never_takes_its_turn_stops_holding_up_the_later_ones() {
    let (mut scheduler, connection) = scheduler();
    let first = scheduler.arrive(connection, Some(dev()));
    let second = scheduler.arrive(connection, Some(dev()));
    assert_eq!(scheduler.on(Event::Turn(second, dev())), []);
    assert_eq!(
        scheduler.on(Event::Finish(first)),
        [Action::Grant(second, dev())]
    );
    // So does one cancelled while it waits for them.
    let third = scheduler.arrive(connection, Some(dev()));
    let fourth = scheduler.arrive(connection, Some(dev()));
    let _ = scheduler.on(Event::EndTurn(second, dev()));
    assert_eq!(scheduler.on(Event::Turn(fourth, dev())), []);
    let _ = scheduler.on(Event::Act(third));
    assert_eq!(
        scheduler.on(Event::Turn(third, dev())),
        [Action::Grant(third, dev())]
    );
    let fifth = scheduler.arrive(connection, Some(dev()));
    let sixth = scheduler.arrive(connection, Some(dev()));
    let _ = scheduler.on(Event::Finish(third));
    let _ = scheduler.on(Event::Finish(fourth));
    assert_eq!(scheduler.on(Event::Turn(sixth, dev())), []);
    assert_eq!(
        scheduler.on(Event::Turn(fifth, dev())),
        [Action::Grant(fifth, dev())]
    );
    assert_eq!(scheduler.on(Event::Cancel(fifth)), []);
    assert_eq!(
        scheduler.on(Event::Finish(fifth)),
        [Action::Grant(sixth, dev())]
    );
}

#[test]
fn a_later_turn_of_the_same_call_joins_the_end_of_the_line() {
    let mut scheduler = Scheduler::default();
    let (one, two) = (scheduler.connection(), scheduler.connection());
    let call = scheduler.arrive(one, Some(dev()));
    assert_eq!(
        scheduler.on(Event::Turn(call, dev())),
        [Action::Grant(call, dev())]
    );
    let other = scheduler.arrive(two, None);
    let _ = scheduler.on(Event::Turn(other, dev()));
    assert_eq!(
        scheduler.on(Event::EndTurn(call, dev())),
        [Action::Grant(other, dev())]
    );
    assert_eq!(scheduler.on(Event::Turn(call, dev())), []);
    assert_eq!(
        scheduler.on(Event::EndTurn(other, dev())),
        [Action::Grant(call, dev())]
    );
}

#[test]
fn idle_lines_are_forgotten() {
    let (mut scheduler, connection) = scheduler();
    let call = scheduler.arrive(connection, Some(dev()));
    let _ = scheduler.on(Event::Turn(call, dev()));
    let _ = scheduler.on(Event::Finish(call));
    assert!(scheduler.lines.is_empty());
    assert!(scheduler.calls.is_empty());
}

/// The scheduler and the state machines it applies decide; they never
/// block, sleep or read a clock.
#[test]
fn the_scheduler_does_no_io() {
    for (file, source) in [
        ("mod.rs", include_str!("mod.rs")),
        ("control.rs", include_str!("control.rs")),
        ("session.rs", include_str!("session.rs")),
    ] {
        // Their own tests may anchor time anywhere.
        let code = source.split("#[cfg(test)]").next().unwrap_or(source);
        for forbidden in FORBIDDEN {
            assert!(!code.contains(forbidden), "{file} uses {forbidden}");
        }
    }
}

const FORBIDDEN: [&str; 11] = [
    "std::sync",
    "std::thread",
    "std::io",
    "std::fs",
    "std::net",
    "std::os",
    "Instant::now",
    "SystemTime",
    "doppel_host",
    "doppel_protocol",
    "tokio",
];
