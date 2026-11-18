import Testing
import Foundation
import CoreGraphics
import DoppelKeys
import DoppelWire
@testable import DoppelAgent

@Test func virtualKeysCoverUSLayout() {
    #expect(VirtualKey(character: "a") == VirtualKey(code: 0))
    #expect(VirtualKey(character: "A") == VirtualKey(code: 0, needsShift: true))
    #expect(VirtualKey(character: "?") == VirtualKey(code: 44, needsShift: true))
    #expect(VirtualKey(character: "\n") == VirtualKey(code: 36))
    #expect(VirtualKey(character: "é") == nil)
    #expect(VirtualKey(named: .f12) == VirtualKey(code: 111))
    #expect(VirtualKey(named: .unspecified) == nil)
}

@Test @MainActor func unimplementedOperationsAreTypedErrors() async {
    let agent = Agent()
    guard case .error(let error) = await agent.handle(.exec(.init())), case .invalidArgument(let bad) = error.kind else {
        Issue.record("expected InvalidArgument for an empty program"); return
    }
    #expect(bad.field == "program")
    guard case .error(let missing) = await agent.handle(nil), case .invalidArgument(let argument) = missing.kind else {
        Issue.record("expected InvalidArgument"); return
    }
    #expect(argument.field == "operation")
}

@Test func errorsMapToTheWire() {
    #expect(AgentError.permissionMissing(.accessibility).wire.permissionMissing.permission == .accessibility)
    #expect(AgentError.invalidArgument(field: "f", reason: "r").wire.invalidArgument.field == "f")
}

@Test @MainActor func actionsOnUnknownSnapshotsAreStale() async {
    // No snapshot was taken, so no id resolves: stale, whatever the permissions.
    let agent = Agent()
    let request = Doppel_Agent_V1_PerformAxAction.with { $0.snapshotID = 42; $0.nodeIndex = 3; $0.press = .init() }
    guard case .error(let error) = await agent.handle(.performAxAction(request)),
          case .staleReference(let stale) = error.kind else {
        Issue.record("expected StaleReference"); return
    }
    #expect(stale.snapshotID == 42 && stale.nodeIndex == 3)
}

@Test func staleAndInvalidStateMapToTheWire() {
    #expect(AgentError.staleReference(snapshotID: 7, nodeIndex: 2).wire.staleReference.snapshotID == 7)
    #expect(AgentError.invalidState("x").wire.invalidState.reason == "x")
}

@Test func hostKeyStrokesPressEachModifierWithItsOwnKey() {
    // cmd+shift+4: control-free chord, pressed in canonical order, released in reverse.
    let stroke = KeyStroke(code: 21, modifiers: [.command, .shift])
    let shift = ModifierKey.shift.flags, command = ModifierKey.command.flags
    #expect(stroke.events == [
        .init(phase: .flagsChanged, keyCode: 56, flags: shift),
        .init(phase: .flagsChanged, keyCode: 55, flags: shift | command),
        .init(phase: .keyDown, keyCode: 21, flags: shift | command),
        .init(phase: .keyUp, keyCode: 21, flags: shift | command),
        .init(phase: .flagsChanged, keyCode: 55, flags: shift),
        .init(phase: .flagsChanged, keyCode: 56, flags: 0),
    ])
    // Every modifier has a distinct key code and a device-dependent bit (fn excepted):
    // sent all as key 56 with no device bit, the guest reads them as one key.
    #expect(Set(ModifierKey.allCases.map(\.keyCode)).count == ModifierKey.allCases.count)
    #expect(ModifierKey.control.keyCode == 59 && ModifierKey.control.flags & 0x0001 != 0)
    #expect(ModifierKey.shift.flags & 0x0002 != 0)
    // A bare key has no flagsChanged events at all.
    #expect(KeyStroke(code: 0, modifiers: []).events.map(\.phase) == [.keyDown, .keyUp])
}

@Test func modifierFlagsShareCGEventFlagsLayout() {
    // The agent posts KeyStroke flags as CGEventFlags: the bits must line up.
    let pairs: [(ModifierKey, CGEventFlags)] = [
        (.command, .maskCommand), (.shift, .maskShift), (.option, .maskAlternate),
        (.control, .maskControl), (.function, .maskSecondaryFn),
    ]
    for (key, mask) in pairs {
        #expect(CGEventFlags(rawValue: UInt64(key.flags)).contains(mask), "\(key)")
        // And no other modifier's device-independent bit leaks in.
        for (other, otherMask) in pairs where other != key {
            #expect(!CGEventFlags(rawValue: UInt64(key.flags)).contains(otherMask), "\(key) has \(other)")
        }
    }
    #expect(ModifierKey.flags(ofKeyCode: 55) == ModifierKey.command.flags)
    #expect(ModifierKey.flags(ofKeyCode: 0) == 0)
    // A chord ends with nothing held: the bug left Command set after cmd+down.
    #expect(KeyStroke(code: 125, modifiers: [.command]).events.last?.flags == 0)
}

@Test func aModifierHoldPressesInOrderAndEndsWithNothingHeld() {
    let hold = ModifierHold([.shift, .command])
    let shift = ModifierKey.shift.flags, command = ModifierKey.command.flags
    #expect(hold.flags == shift | command)
    #expect(hold.press.map(\.keyCode) == [56, 55])
    #expect(hold.release.map(\.keyCode) == [55, 56])
    #expect(hold.release.last?.flags == 0)
    #expect(ModifierHold([]).press.isEmpty && ModifierHold([]).flags == 0)
}

@Test func pointerEventsDecodeOnceAndRejectWhatCantBePosted() throws {
    func pointer(_ phase: Doppel_Common_V1_PointerPhase, _ button: Doppel_Common_V1_MouseButton, clicks: UInt32 = 0) -> Doppel_Common_V1_Pointer {
        .with { $0.phase = phase; $0.button = button; $0.clickCount = clicks }
    }
    #expect(try PointerAction(pointer(.press, .left, clicks: 2)) == .press(.left, clicks: 2))
    #expect(try PointerAction(pointer(.release, .right, clicks: 9)) == .release(.right, clicks: 3))
    #expect(try PointerAction(pointer(.move, .unspecified)) == .move(held: nil))
    #expect(try PointerAction(pointer(.move, .middle)) == .move(held: .middle))
    #expect(throws: PointerAction.Invalid.self) { try PointerAction(pointer(.press, .unspecified)) }
    #expect(throws: PointerAction.Invalid.self) { try PointerAction(pointer(.unspecified, .left)) }
}

// Exec runs on the host in tests: real processes, no guest needed.

@Test func execReportsExitStatusAndOutput() async throws {
    let result = try await GuestExec.run(.with {
        $0.program = "/bin/sh"; $0.arguments = ["-c", "read x; echo out:$x; echo err >&2; exit 3"]
        $0.stdin = Data("hi\n".utf8); $0.timeoutMs = 5_000; $0.maxOutputBytes = 1_000
    })
    #expect(result.exitCode == 3)
    #expect(result.stdout == Data("out:hi\n".utf8))
    #expect(result.stderr == Data("err\n".utf8))
    #expect(result.completeness == .full)
}

@Test func execStopsAtTheTimeout() async throws {
    let started = Date()
    let result = try await GuestExec.run(.with { $0.program = "/bin/sleep"; $0.arguments = ["10"]; $0.timeoutMs = 300 })
    guard case .timedOut = result.termination else { Issue.record("expected a timeout, got \(String(describing: result.termination))"); return }
    #expect(Date().timeIntervalSince(started) < 3)
}

@Test func cancellingAnExecStopsItsProgram() async throws {
    let started = Date()
    let task = Task {
        try await GuestExec.run(.with { $0.program = "/bin/sleep"; $0.arguments = ["30"]; $0.timeoutMs = 60_000 })
    }
    try await Task.sleep(for: .milliseconds(200))
    task.cancel()
    let result = try await task.value
    #expect(result.signal == SIGTERM)
    #expect(Date().timeIntervalSince(started) < 5)
}

@Test func aPeerHangingUpIsNoticedAndDataIsNot() async throws {
    func pair() -> (Int32, Int32) {
        var fds: [Int32] = [0, 0]
        precondition(socketpair(AF_UNIX, SOCK_STREAM, 0, &fds) == 0)
        return (fds[0], fds[1])
    }
    final class Hits: @unchecked Sendable {
        private let lock = NSLock(); private var n = 0
        func hit() { lock.lock(); n += 1; lock.unlock() }
        var count: Int { lock.lock(); defer { lock.unlock() }; return n }
    }

    let (watched, peer) = pair()
    let hungUp = Hits()
    let watch = HangUpWatch(fd: watched) { hungUp.hit() }
    close(peer)
    try await Task.sleep(for: .milliseconds(400))
    watch.end()
    close(watched)
    #expect(hungUp.count == 1)

    let (talked, other) = pair()
    let talking = Hits()
    let quiet = HangUpWatch(fd: talked) { talking.hit() }
    var byte: UInt8 = 7
    _ = write(other, &byte, 1)
    try await Task.sleep(for: .milliseconds(400))
    close(other)
    try await Task.sleep(for: .milliseconds(300))
    quiet.end()
    #expect(talking.count == 0, "data ends the watch, unread")
    #expect(read(talked, &byte, 1) == 1 && byte == 7)
    close(talked)
}

@Test func execTruncatesAndStillDrains() async throws {
    // 200 KB through a 1 KB limit: kept 1 KB, the rest dropped, and the program wasn't blocked.
    let result = try await GuestExec.run(.with {
        $0.program = "/bin/sh"; $0.arguments = ["-c", "head -c 200000 /dev/zero"]; $0.timeoutMs = 5_000; $0.maxOutputBytes = 1_024
    })
    #expect(result.exitCode == 0)
    #expect(result.stdout.count == 1_024)
    #expect(result.completeness == .truncated)
}

@Test func execSurvivesAProgramThatIgnoresItsInput() async throws {
    // 1 MB fills the pipe, so the write is still pending when the program exits and
    // finds no reader. That write must fail quietly, not raise SIGPIPE in the agent.
    let result = try await GuestExec.run(.with {
        $0.program = "/usr/bin/true"; $0.stdin = Data(count: 1_000_000); $0.timeoutMs = 5_000
    })
    #expect(result.exitCode == 0)
}

@Test func programsStartWithDefaultSignalHandling() async throws {
    // The agent ignores SIGPIPE; a program it runs must not inherit that, or a
    // pipeline like `yes | head -1` never ends its producer.
    signal(SIGPIPE, SIG_IGN)
    let result = try await GuestExec.run(.with {
        $0.program = "/usr/bin/perl"; $0.arguments = ["-e", "print $SIG{PIPE} // 'DEFAULT'"]; $0.timeoutMs = 5_000
    })
    #expect(result.exitCode == 0)
    #expect(String(decoding: result.stdout, as: UTF8.self) == "DEFAULT")
}

@Test func execRunsInTheGivenDirectoryWithTheAgentsEnvironment() async throws {
    let result = try await GuestExec.run(.with {
        $0.program = "/bin/sh"; $0.arguments = ["-c", "pwd; echo ${HOME:+home set}"]
        $0.workingDirectory = "/private/tmp"; $0.timeoutMs = 5_000
    })
    #expect(String(decoding: result.stdout, as: UTF8.self) == "/private/tmp\nhome set\n")
}

@Test func manyShortProgramsAreEachNoticedPromptly() async throws {
    // Foundation's Process missed exits until another child was spawned (seen
    // live: replies 13 s late). Each run must end when its program does.
    for _ in 0 ..< 20 {
        let started = Date()
        let result = try await GuestExec.run(.with { $0.program = "/usr/bin/true"; $0.timeoutMs = 5_000 })
        #expect(result.exitCode == 0)
        #expect(Date().timeIntervalSince(started) < 1)
    }
}

@Test func execReportsTheSignalThatEndedAProgram() async throws {
    let result = try await GuestExec.run(.with { $0.program = "/bin/sh"; $0.arguments = ["-c", "kill -9 $$"]; $0.timeoutMs = 5_000 })
    #expect(result.signal == 9)
}

@Test func execRefusesMissingAndRelativePrograms() async {
    for program in ["/no/such/program", "sw_vers"] {
        do {
            _ = try await GuestExec.run(.with { $0.program = program; $0.timeoutMs = 1_000 })
            Issue.record("\(program) should have failed")
        } catch AgentError.invalidArgument(let field, _) {
            #expect(field == "program")
        } catch {
            Issue.record("unexpected \(error)")
        }
    }
}

// MARK: - Files

private func scratchDirectory() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("doppel-files-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
    return url
}

private func entries(_ directory: URL) throws -> [String] {
    try FileManager.default.contentsOfDirectory(atPath: directory.path).sorted()
}

@Test @MainActor func filesRoundTripThroughTheAgent() async throws {
    let directory = try scratchDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let path = directory.appendingPathComponent("blob.bin").path
    let bytes = Data((0..<70_000).map { UInt8(truncatingIfNeeded: $0 &* 31) })
    let agent = Agent()

    guard case .done = await agent.handle(.writeFile(.with { $0.path = path; $0.data = bytes; $0.mode = 0o750; $0.policy = .createNew })) else {
        Issue.record("write failed"); return
    }
    guard case .fileContents(let contents) = await agent.handle(.readFile(.with { $0.path = path; $0.maxBytes = 70_000 })) else {
        Issue.record("read failed"); return
    }
    #expect(contents.data == bytes)
    let mode = try FileManager.default.attributesOfItem(atPath: path)[.posixPermissions] as? Int
    #expect(mode == 0o750)
    // No temporary file is left behind.
    #expect(try entries(directory) == ["blob.bin"])
}

@Test func createNewNeverReplacesAndReplaceDoes() throws {
    let directory = try scratchDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let path = directory.appendingPathComponent("f").path
    try GuestFiles.write(.with { $0.path = path; $0.data = Data("one".utf8); $0.policy = .createNew })
    do {
        try GuestFiles.write(.with { $0.path = path; $0.data = Data("two".utf8); $0.policy = .createNew })
        Issue.record("CREATE_NEW replaced an existing file")
    } catch AgentError.invalidState {
    } catch {
        Issue.record("unexpected \(error)")
    }
    #expect(try Data(contentsOf: URL(fileURLWithPath: path)) == Data("one".utf8))
    try GuestFiles.write(.with { $0.path = path; $0.data = Data("three".utf8); $0.policy = .replace })
    #expect(try Data(contentsOf: URL(fileURLWithPath: path)) == Data("three".utf8))
    let mode = try FileManager.default.attributesOfItem(atPath: path)[.posixPermissions] as? Int
    #expect(mode == 0o644, "mode 0 means 0644")
    #expect(try entries(directory) == ["f"])
}

@Test func readRefusesOversizedFilesInsteadOfTruncating() throws {
    let directory = try scratchDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let path = directory.appendingPathComponent("big").path
    try Data(count: 101).write(to: URL(fileURLWithPath: path))
    #expect(try GuestFiles.read(.with { $0.path = path; $0.maxBytes = 101 }).data.count == 101)
    do {
        _ = try GuestFiles.read(.with { $0.path = path; $0.maxBytes = 100 })
        Issue.record("an oversized file was read")
    } catch AgentError.invalidArgument(let field, _) {
        #expect(field == "max_bytes")
    } catch {
        Issue.record("unexpected \(error)")
    }
}

@Test func badPathsModesAndPoliciesAreRefused() throws {
    let directory = try scratchDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let cases: [(String, () throws -> Void)] = [
        ("path", { _ = try GuestFiles.read(.with { $0.path = "relative/file" }) }),
        ("path", { _ = try GuestFiles.read(.with { $0.path = directory.appendingPathComponent("missing").path }) }),
        ("path", { _ = try GuestFiles.read(.with { $0.path = directory.path }) }),
        ("path", { try GuestFiles.write(.with { $0.path = directory.appendingPathComponent("no/such/dir/f").path; $0.policy = .replace }) }),
        ("mode", { try GuestFiles.write(.with { $0.path = directory.appendingPathComponent("s").path; $0.mode = 0o4755; $0.policy = .replace }) }),
        ("policy", { try GuestFiles.write(.with { $0.path = directory.appendingPathComponent("p").path }) }),
    ]
    for (expected, run) in cases {
        do {
            try run()
            Issue.record("expected InvalidArgument(\(expected))")
        } catch AgentError.invalidArgument(let field, _) {
            #expect(field == expected)
        } catch {
            Issue.record("unexpected \(error)")
        }
    }
    #expect(try entries(directory) == [], "refused writes leave nothing behind")
}

// Pushed events: coalesced per kind, sent only to subscribed connections.

@Test func anOutboxKeepsOnlyTheLatestEventOfEachKind() {
    let outbox = EventOutbox()
    outbox.post(.with { $0.uiChanged.pid = 1 })
    outbox.post(.with { $0.frontmostAppChanged.app.pid = 2 })
    outbox.post(.with { $0.uiChanged.pid = 3 })
    let taken = outbox.take() ?? []
    #expect(taken.map(\.uiChanged.pid) == [3, 0])
    #expect(taken.last?.frontmostAppChanged.app.pid == 2)
    outbox.close()
    #expect(outbox.take() == nil)
}

/// The host's side of a connection: frames in, frames out, over a socketpair.
private final class Host {
    let fd: Int32
    private var decoder = FrameDecoder<Doppel_Agent_V1_AgentFrame>()

    init(serving events: EventHub) {
        var fds: [Int32] = [0, 0]
        precondition(socketpair(AF_UNIX, SOCK_STREAM, 0, &fds) == 0)
        fd = fds[0]
        let agentSide = fds[1]
        Thread.detachNewThread {
            AgentConnection(fd: agentSide, events: events).run(
                hello: .with { $0.protocolVersion = 1 },
                handler: { _ in .done(.init()) }
            )
        }
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    func send(_ frame: Doppel_Agent_V1_AgentFrame) throws {
        let data = try FrameEncoder().encode(frame)
        _ = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
    }

    /// The next frame, or nil after 2 s without one.
    func next() throws -> Doppel_Agent_V1_AgentFrame? {
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            if case .recognized(let frame, _) = try decoder.nextFrame() { return frame }
            let n = read(fd, &buffer, buffer.count)
            guard n > 0 else { return nil }
            decoder.push(buffer[0..<n])
        }
    }
}

@Test func onlyASubscribedConnectionGetsEvents() throws {
    let events = EventHub()
    let host = Host(serving: events)
    #expect(try host.next()?.hello.protocolVersion == 1)
    try host.send(.with { $0.hello.protocolVersion = 1 })

    // Not subscribed: a published event never arrives; the reply does.
    events.publish(.with { $0.uiChanged.pid = 9 })
    try host.send(.with { $0.request.id = 1; $0.request.getStatus = .init() })
    #expect(try host.next()?.response.id == 1)

    try host.send(.with { $0.request.id = 2; $0.request.subscribe = .init() })
    let subscribed = try host.next()
    #expect(subscribed?.response.id == 2)
    #expect(subscribed?.response.done != nil)
    events.publish(.with { $0.uiChanged.pid = 42 })
    #expect(try host.next()?.event.uiChanged.pid == 42)
    close(host.fd)
}

@Test @MainActor func aProbeWithNoSizeOrAColourPast8BitsIsRefused() async {
    let agent = Agent()
    let flat = await agent.handle(.showProbe(.with {
        $0.frame = .with { $0.size = .with { $0.width = 0; $0.height = 4 } }
    }))
    guard case .error(let error) = flat else { Issue.record("expected an error, got \(flat)"); return }
    #expect(error.invalidArgument.field == "frame")
    let tooRed = await agent.handle(.showProbe(.with {
        $0.frame = .with { $0.size = .with { $0.width = 4; $0.height = 4 } }
        $0.color = .with { $0.red = 256 }
    }))
    guard case .error(let error) = tooRed else { Issue.record("expected an error, got \(tooRed)"); return }
    #expect(error.invalidArgument.field == "color")
    // Hiding with none shown is fine.
    guard case .done = await agent.handle(.hideProbe(.init())) else { Issue.record("hide failed"); return }
}
