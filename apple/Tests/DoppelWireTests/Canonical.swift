// Independent Swift construction of the canonical streams that
// `crates/doppel-protocol/tests/golden.rs` builds. Keep the two in lockstep: the
// golden tests require both to encode to the committed bytes.

import Foundation
@testable import DoppelWire

enum Canonical {
    static var agentStream: [Doppel_Agent_V1_AgentFrame] {
        var hello = Doppel_Agent_V1_AgentFrame()
        hello.hello.protocolVersion = WireVersion.agent
        hello.hello.softwareVersion = "doppel-agent 0.1.0"
        hello.hello.capabilities = [.screenshot, .input, .axSnapshot, .axObserve]

        var click = Doppel_Agent_V1_AgentFrame()
        click.request.id = 1
        click.request.input.click.at.x = 640.5
        click.request.input.click.at.y = 400
        click.request.input.click.button = .left
        click.request.input.click.count = 2

        var chord = Doppel_Agent_V1_AgentFrame()
        chord.request.id = 2
        chord.request.input.key.modifiers = [.command, .shift]
        chord.request.input.key.key.character = "é"

        var ask = Doppel_Agent_V1_AgentFrame()
        ask.request.id = 3
        ask.request.takeAxSnapshot.focusedWindow = Doppel_Agent_V1_Empty()
        ask.request.takeAxSnapshot.maxNodes = 2000
        ask.request.takeAxSnapshot.maxDepth = 64
        ask.request.takeAxSnapshot.deadlineMs = 500
        ask.request.takeAxSnapshot.menus = .exclude
        ask.request.takeAxSnapshot.retention = .observation

        var window = Doppel_Agent_V1_AxNode()
        window.role = "AXWindow"
        window.title = "Downloads"
        window.frame.origin.x = 0
        window.frame.origin.y = 25
        window.frame.size.width = 800
        window.frame.size.height = 600
        window.flags.focused = true
        var field = Doppel_Agent_V1_AxNode()
        field.parent = 0
        field.role = "AXTextField"
        field.subrole = "AXSearchField"
        field.value.text = ""
        field.flags.enabled = false
        field.actions = ["AXPress", "AXConfirm"]
        var snapshot = Doppel_Agent_V1_AgentFrame()
        snapshot.response.id = 3
        snapshot.response.axSnapshot.snapshotID = 42
        snapshot.response.axSnapshot.app.pid = 501
        snapshot.response.axSnapshot.app.bundleID = "com.apple.finder"
        snapshot.response.axSnapshot.app.name = "Finder"
        snapshot.response.axSnapshot.nodes = [window, field]
        snapshot.response.axSnapshot.truncation = .complete

        var stale = Doppel_Agent_V1_AgentFrame()
        stale.response.id = 4
        stale.response.error.staleReference.snapshotID = 41
        stale.response.error.staleReference.nodeIndex = 7

        var exec = Doppel_Agent_V1_AgentFrame()
        exec.response.id = 5
        exec.response.execResult.exitCode = 0
        exec.response.execResult.stdout = Data("hello\n".utf8)
        exec.response.execResult.completeness = .full

        var event = Doppel_Agent_V1_AgentFrame()
        event.event.permissionsChanged.granted = [.accessibility]

        var subscribe = Doppel_Agent_V1_AgentFrame()
        subscribe.request.id = 6
        subscribe.request.subscribe = Doppel_Agent_V1_Subscribe()

        var frontmost = Doppel_Agent_V1_AgentFrame()
        frontmost.event.frontmostAppChanged.app.pid = 612
        frontmost.event.frontmostAppChanged.app.bundleID = "com.apple.TextEdit"
        frontmost.event.frontmostAppChanged.app.name = "TextEdit"

        var changed = Doppel_Agent_V1_AgentFrame()
        changed.event.uiChanged.pid = 612

        var showProbe = Doppel_Agent_V1_AgentFrame()
        showProbe.request.id = 7
        showProbe.request.showProbe = .with {
            $0.frame = .with { $0.origin = .with { $0.x = 0; $0.y = 0 }; $0.size = .with { $0.width = 4; $0.height = 4 } }
            $0.color = .with { $0.red = 0; $0.green = 0; $0.blue = 255 }
        }

        var hideProbe = Doppel_Agent_V1_AgentFrame()
        hideProbe.request.id = 8
        hideProbe.request.hideProbe = Doppel_Agent_V1_HideProbe()

        return [hello, click, chord, ask, snapshot, stale, exec, event, subscribe, frontmost, changed, showProbe, hideProbe]
    }

    static var vmmStream: [Doppel_Vmm_V1_VmmFrame] {
        var request = Doppel_Vmm_V1_VmmFrame()
        request.request.id = 9
        request.request.saveState.path = "/tmp/checkpoints/before-install/memory.vzsave"

        var response = Doppel_Vmm_V1_VmmFrame()
        response.response.id = 9
        response.response.stateSaved.bytes = 1_744_830_464

        var state = Doppel_Vmm_V1_VmmFrame()
        state.event.stateChanged.runState = .paused

        var progress = Doppel_Vmm_V1_VmmFrame()
        progress.event.installProgress.fraction = 0.25

        var openDisplay = Doppel_Vmm_V1_VmmFrame()
        openDisplay.request.id = 10
        openDisplay.request.openDisplay = .init()

        var opened = Doppel_Vmm_V1_VmmFrame()
        opened.response.id = 10
        opened.response.displayOpened.layout = .with {
            $0.widthPixels = 2560
            $0.heightPixels = 1600
            $0.displayPoints = .with { $0.width = 1280; $0.height = 800 }
            $0.slotCount = 3
            $0.slotStride = 6_160_384
            $0.lumaOffset = 64
            $0.lumaBytesPerRow = 2560
            $0.chromaOffset = 4_096_064
            $0.chromaBytesPerRow = 2560
        }

        var frame = Doppel_Vmm_V1_VmmFrame()
        frame.event.displayFrame = .with { $0.slot = 2; $0.sequence = 41 }

        var drag = Doppel_Vmm_V1_VmmFrame()
        drag.request.id = 11
        drag.request.hostInput.pointer = .with {
            $0.phase = .move
            $0.at = .with { $0.x = 640.5; $0.y = 400 }
            $0.button = .left
            $0.modifiers = [.shift]
        }

        var stop = Doppel_Vmm_V1_VmmFrame()
        stop.request.id = 12
        stop.request.stopRecording.path = "/tmp/runs/dev-001.mp4"

        var stopped = Doppel_Vmm_V1_VmmFrame()
        stopped.response.id = 12
        stopped.response.recordingStopped = .with {
            $0.path = "/tmp/runs/dev-001.mp4"
            $0.frames = 1_812
            $0.seconds = 95.5
            $0.bytes = 48_000_000
        }

        var openEncoded = Doppel_Vmm_V1_VmmFrame()
        openEncoded.request.id = 13
        openEncoded.request.openEncodedDisplay = .init()

        var encodedOpened = Doppel_Vmm_V1_VmmFrame()
        encodedOpened.response.id = 13
        encodedOpened.response.encodedDisplayOpened = .with {
            $0.widthPixels = 2560
            $0.heightPixels = 1600
            $0.displayPoints = .with { $0.width = 1280; $0.height = 800 }
        }

        var encoded = Doppel_Vmm_V1_VmmFrame()
        encoded.event.encodedFrame = .with {
            $0.sequence = 1
            $0.key = true
            $0.parameterSets = [Data([0x67, 0x64, 0x00, 0x28]), Data([0x68, 0xEE])]
            $0.data = Data([0, 0, 0, 3, 0x65, 0x88, 0x84])
        }

        var shown = Doppel_Vmm_V1_VmmFrame()
        shown.response.id = 14
        shown.response.state = .with {
            $0.runState = .running
            $0.window = .shown
        }

        var sample = Doppel_Vmm_V1_VmmFrame()
        sample.request.id = 15
        sample.request.sampleDisplay.at = .with { $0.x = 2; $0.y = 2 }

        var sampled = Doppel_Vmm_V1_VmmFrame()
        sampled.response.id = 15
        sampled.response.displaySample.color = .with { $0.red = 236; $0.green = 237; $0.blue = 238 }

        var hello = Doppel_Vmm_V1_VmmFrame()
        hello.hello.protocolVersion = WireVersion.vmm
        hello.hello.softwareVersion = "doppel-vmm 0.1.0"

        return [
            request, response, state, progress, openDisplay, opened, frame, drag, stop, stopped,
            openEncoded, encodedOpened, encoded, shown, sample, sampled, hello,
        ]
    }

    static var machineSpec: Doppel_Bundle_V1_MachineSpec {
        var spec = Doppel_Bundle_V1_MachineSpec()
        spec.cpuCount = 4
        spec.memoryBytes = 4 << 30
        spec.diskBytes = 64 << 30
        spec.display.widthPixels = 2560
        spec.display.heightPixels = 1600
        spec.display.pixelsPerInch = 220
        spec.network.nat.macAddress = "aa:d9:39:38:2f:8a"
        spec.hardwareModel = Data([1, 2, 3, 4])
        spec.machineIdentifier = Data([9, 8, 7])
        var tools = Doppel_Bundle_V1_SharedDirectory()
        tools.name = "tools"
        tools.hostPath = "/Users/someone/doppel/tools"
        tools.access = .readOnly
        spec.sharedDirectories = [tools]
        return spec
    }
}
