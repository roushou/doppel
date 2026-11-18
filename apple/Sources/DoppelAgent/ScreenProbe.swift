// A small square above every window, so the host can tell when its view of
// the screen shows what the guest draws now (see ShowProbe in agent.proto).
//
// A borderless panel that never activates: it doesn't take focus, change
// the frontmost app or catch the pointer. One at a time; it removes itself
// after `lasts`, so a host that stops midway leaves it no longer.

import AppKit
import DoppelWire

@MainActor
final class ScreenProbe {
    static let lasts: TimeInterval = 30

    private var panel: NSPanel?
    private var expiry: Timer?

    func show(_ request: Doppel_Agent_V1_ShowProbe) throws {
        let frame = request.frame
        guard frame.size.width > 0, frame.size.height > 0,
              [frame.origin.x, frame.origin.y, frame.size.width, frame.size.height].allSatisfy(\.isFinite)
        else { throw AgentError.invalidArgument(field: "frame", reason: "needs a finite origin and a positive size") }
        let color = request.color
        guard [color.red, color.green, color.blue].allSatisfy({ $0 <= 255 }) else {
            throw AgentError.invalidArgument(field: "color", reason: "channels are 0 to 255")
        }
        // Display points have a top-left origin on the main display; AppKit's
        // screen coordinates a bottom-left one.
        guard let screen = NSScreen.screens.first else { throw AgentError.failed("no screen to draw on") }
        let rect = NSRect(x: frame.origin.x,
                          y: screen.frame.maxY - frame.origin.y - frame.size.height,
                          width: frame.size.width, height: frame.size.height)
        let panel = self.panel ?? Self.makePanel()
        self.panel = panel
        panel.backgroundColor = NSColor(srgbRed: CGFloat(color.red) / 255, green: CGFloat(color.green) / 255,
                                        blue: CGFloat(color.blue) / 255, alpha: 1)
        panel.setFrame(rect, display: true)
        panel.orderFrontRegardless()
        expiry?.invalidate()
        expiry = Timer.scheduledTimer(withTimeInterval: Self.lasts, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.hide() }
        }
    }

    func hide() {
        expiry?.invalidate()
        expiry = nil
        panel?.orderOut(nil)
    }

    private static func makePanel() -> NSPanel {
        let panel = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        panel.ignoresMouseEvents = true
        panel.isOpaque = true
        panel.hasShadow = false
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        // Gone in one frame when hidden: a fade would linger in screenshots.
        panel.animationBehavior = .none
        return panel
    }
}
