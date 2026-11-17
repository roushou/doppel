// Decodes the fixtures written by `crates/doppel-protocol/tests/golden.rs`
// (and the bundle's spec file, by `tests/bundle.rs`), checks
// the values Rust put there, and requires Swift's independently built
// canonical frames (Canonical.swift) to encode to exactly the same bytes.

import Foundation
import Testing
@testable import DoppelWire

private func fixture(_ name: String, in dir: String = "wire") throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("fixtures/\(dir)/\(name)"))
}

private func decodeAll<M: Envelope>(_ data: Data, as: M.Type) throws -> [M] {
    var decoder = FrameDecoder<M>()
    decoder.push(data)
    var out: [M] = []
    while let frame = try decoder.nextFrame() {
        guard case .recognized(let message, _) = frame else { Issue.record("not recognized: \(frame)"); continue }
        out.append(message)
    }
    #expect(decoder.pendingBytes == 0)
    return out
}

@Suite struct GoldenFixtures {
    @Test func agentStreamDecodesToRustValuesAndReencodesIdentically() throws {
        let bytes = try fixture("agent_stream.v1.bin")
        let frames = try decodeAll(bytes, as: Doppel_Agent_V1_AgentFrame.self)
        try #require(frames.count == 11)

        #expect(frames[0].hello.protocolVersion == 1)
        #expect(frames[0].hello.softwareVersion == "doppel-agent 0.1.0")
        #expect(frames[0].hello.capabilities == [.screenshot, .input, .axSnapshot, .axObserve])

        let click = frames[1].request.input.click
        #expect(frames[1].request.id == 1)
        #expect(click.at.x == 640.5 && click.at.y == 400 && click.button == .left && click.count == 2)

        let chord = frames[2].request.input.key
        #expect(chord.modifiers == [.command, .shift])
        #expect(chord.key.character == "é")

        let ask = frames[3].request.takeAxSnapshot
        #expect(ask.scope == .focusedWindow(Doppel_Agent_V1_Empty()))
        #expect(ask.maxNodes == 2000 && ask.maxDepth == 64 && ask.deadlineMs == 500 && ask.menus == .exclude && ask.retention == .observation)

        let snapshot = frames[4].response.axSnapshot
        #expect(snapshot.snapshotID == 42 && snapshot.app.bundleID == "com.apple.finder" && snapshot.truncation == .complete)
        try #require(snapshot.nodes.count == 2)
        #expect(!snapshot.nodes[0].hasParent)
        #expect(snapshot.nodes[0].flags.focused && !snapshot.nodes[0].flags.hasEnabled)
        #expect(snapshot.nodes[0].frame.size.width == 800)
        #expect(snapshot.nodes[1].hasParent && snapshot.nodes[1].parent == 0)
        #expect(snapshot.nodes[1].subrole == "AXSearchField")
        #expect(snapshot.nodes[1].value.text == "")
        #expect(snapshot.nodes[1].flags.hasEnabled && !snapshot.nodes[1].flags.enabled)
        #expect(snapshot.nodes[1].actions == ["AXPress", "AXConfirm"])

        #expect(frames[5].response.error.staleReference.snapshotID == 41)
        #expect(frames[5].response.error.staleReference.nodeIndex == 7)

        #expect(frames[6].response.execResult.exitCode == 0)
        #expect(frames[6].response.execResult.stdout == Data("hello\n".utf8))

        #expect(frames[7].event.permissionsChanged.granted == [.accessibility])

        #expect(frames[8].request.id == 6 && frames[8].request.subscribe == Doppel_Agent_V1_Subscribe())
        #expect(frames[9].event.frontmostAppChanged.app.pid == 612)
        #expect(frames[9].event.frontmostAppChanged.app.bundleID == "com.apple.TextEdit")
        #expect(frames[10].event.uiChanged.pid == 612)

        // The real cross-language check: values built independently in Swift
        // decode-equal Rust's, and encode to Rust's exact bytes.
        #expect(frames == Canonical.agentStream)
        let swiftBytes = try Canonical.agentStream.reduce(into: Data()) { $0.append(try FrameEncoder().encode($1)) }
        #expect(swiftBytes == bytes)
    }

    @Test func vmmStreamDecodesToRustValuesAndReencodesIdentically() throws {
        let bytes = try fixture("vmm_stream.v1.bin")
        let frames = try decodeAll(bytes, as: Doppel_Vmm_V1_VmmFrame.self)
        try #require(frames.count == 14)
        #expect(frames[0].request.saveState.path == "/tmp/checkpoints/before-install/memory.vzsave")
        #expect(frames[1].response.stateSaved.bytes == 1_744_830_464)
        #expect(frames[2].event.stateChanged.runState == .paused)
        #expect(frames[3].event.installProgress.fraction == 0.25)
        #expect(frames[5].response.displayOpened.layout.chromaOffset == 4_096_064)
        #expect(frames[6].event.displayFrame.sequence == 41)
        #expect(frames[7].request.hostInput.pointer.modifiers == [.shift])
        #expect(frames[8].request.stopRecording.hasPath)
        #expect(frames[11].response.encodedDisplayOpened.displayPoints.width == 1280)
        #expect(frames[12].event.encodedFrame.parameterSets.count == 2)
        #expect(frames[9].response.recordingStopped.seconds == 95.5)
        #expect(frames[13].response.state.window == .shown)

        #expect(frames == Canonical.vmmStream)
        let swiftBytes = try Canonical.vmmStream.reduce(into: Data()) { $0.append(try FrameEncoder().encode($1)) }
        #expect(swiftBytes == bytes)
    }

    @Test func machineSpecWrittenByRustIsReadableAndIdentical() throws {
        let bytes = try fixture("spec.v1.bin", in: "bundle")
        let spec = try Doppel_Bundle_V1_MachineSpec(serializedBytes: bytes)
        #expect(spec.network.nat.macAddress == "aa:d9:39:38:2f:8a")
        #expect(spec.sharedDirectories.first?.access == .readOnly)
        #expect(spec == Canonical.machineSpec)
        let swiftBytes: Data = try Canonical.machineSpec.serializedBytes()
        #expect(swiftBytes == bytes)
    }
}
