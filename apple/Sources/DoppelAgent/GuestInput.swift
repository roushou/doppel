// Input inside the guest through CGEvent (needs Accessibility).
// Coordinates are global display points, top-left origin: CGEvent's own space.
// Unlike host input, typing uses Unicode strings, so any character works,
// whatever the guest's keyboard layout.

import CoreGraphics
import Foundation
import ApplicationServices
import DoppelKeys
import DoppelWire

enum GuestInput {
    static func send(_ input: Doppel_Common_V1_Input) throws {
        guard AXIsProcessTrusted() else { throw AgentError.permissionMissing(.accessibility) }
        let source = CGEventSource(stateID: .hidSystemState)
        switch input.action {
        case .click(let click):
            let at = try point(click.at, field: "click.at")
            let (down, up, button): (CGEventType, CGEventType, CGMouseButton) = switch click.button {
            case .right: (.rightMouseDown, .rightMouseUp, .right)
            case .middle: (.otherMouseDown, .otherMouseUp, .center)
            case .left, .unspecified, .UNRECOGNIZED: (.leftMouseDown, .leftMouseUp, .left)
            }
            mouse(.mouseMoved, at: at, button: button, source: source)
            for n in 1...max(1, Int(click.count)) {
                mouse(down, at: at, button: button, clickState: n, source: source)
                mouse(up, at: at, button: button, clickState: n, source: source)
            }
        case .moveTo(let to):
            mouse(.mouseMoved, at: try point(to, field: "move_to"), button: .left, source: source)
        case .pointer(let pointer):
            let action: PointerAction
            do { action = try PointerAction(pointer) } catch let invalid as PointerAction.Invalid {
                throw AgentError.invalidArgument(field: "pointer", reason: invalid.description)
            }
            let at = try point(pointer.at, field: "pointer.at")
            var held: UInt = 0
            for modifier in pointer.modifiers {
                guard let key = ModifierKey(modifier) else {
                    throw AgentError.invalidArgument(field: "pointer.modifiers", reason: "unknown modifier \(modifier)")
                }
                held |= key.flags
            }
            let (type, button, clicks): (CGEventType, CGMouseButton, Int) = switch action {
            case .press(.left, let n): (.leftMouseDown, .left, n)
            case .press(.right, let n): (.rightMouseDown, .right, n)
            case .press(.middle, let n): (.otherMouseDown, .center, n)
            case .release(.left, let n): (.leftMouseUp, .left, n)
            case .release(.right, let n): (.rightMouseUp, .right, n)
            case .release(.middle, let n): (.otherMouseUp, .center, n)
            case .move(held: nil): (.mouseMoved, .left, 0)
            case .move(held: .left): (.leftMouseDragged, .left, 1)
            case .move(held: .right): (.rightMouseDragged, .right, 1)
            case .move(held: .middle): (.otherMouseDragged, .center, 1)
            }
            mouse(type, at: at, button: button, clickState: clicks, flags: CGEventFlags(rawValue: UInt64(held)), source: source)
        case .drag(let drag):
            let from = try point(drag.from, field: "drag.from"), to = try point(drag.to, field: "drag.to")
            mouse(.mouseMoved, at: from, button: .left, source: source)
            mouse(.leftMouseDown, at: from, button: .left, clickState: 1, source: source)
            let steps = max(2, min(30, Int(hypot(to.x - from.x, to.y - from.y) / 12)))
            for step in 1...steps {
                let t = CGFloat(step) / CGFloat(steps)
                mouse(.leftMouseDragged, at: CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t),
                      button: .left, clickState: 1, source: source)
                usleep(8_000)
            }
            mouse(.leftMouseUp, at: to, button: .left, clickState: 1, source: source)
        case .scroll(let scroll):
            mouse(.mouseMoved, at: try point(scroll.at, field: "scroll.at"), button: .left, source: source)
            let wheel = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                                wheel1: Int32(scroll.dy), wheel2: Int32(scroll.dx), wheel3: 0)
            wheel?.flags = []
            wheel?.post(tap: .cghidEventTap)
        case .key(let chord):
            var modifiers = Set<ModifierKey>()
            for modifier in chord.modifiers {
                guard let key = ModifierKey(modifier) else {
                    throw AgentError.invalidArgument(field: "key.modifiers", reason: "unknown modifier")
                }
                modifiers.insert(key)
            }
            let code: UInt16
            switch chord.key.key {
            case .named(let named):
                guard let key = VirtualKey(named: named) else { throw AgentError.invalidArgument(field: "key", reason: "unknown named key") }
                code = key.code
            case .character(let text):
                guard text.count == 1, let c = text.first, let key = VirtualKey(character: c) else {
                    throw AgentError.unsupported("key chord with \(text.debugDescription); use type for text, or a virtual key code")
                }
                code = key.code
                if key.needsShift { modifiers.insert(.shift) }
            case .virtualKeyCode(let raw):
                guard raw <= UInt16.max else { throw AgentError.invalidArgument(field: "key.virtual_key_code", reason: "out of range") }
                code = UInt16(raw)
            case nil:
                throw AgentError.invalidArgument(field: "key", reason: "missing")
            }
            // Press and release each modifier, so the chord ends with nothing
            // held. Posting only key down/up with flags left Command "held" in
            // the system's modifier state: later clicks went out as cmd-clicks,
            // which don't bring a background window forward (seen live).
            for step in KeyStroke(code: code, modifiers: modifiers).events {
                let isDown = step.phase == .keyDown || (step.phase == .flagsChanged && step.flags & ModifierKey.flags(ofKeyCode: step.keyCode) != 0)
                guard let event = CGEvent(keyboardEventSource: source, virtualKey: step.keyCode, keyDown: isDown) else { continue }
                if step.phase == .flagsChanged { event.type = .flagsChanged }
                event.flags = CGEventFlags(rawValue: UInt64(step.flags))
                event.post(tap: .cghidEventTap)
            }
        case .typeText(let typing):
            for character in typing.text {
                if character == "\n" || character == "\r\n" {
                    // Explicit empty flags: an event from a .hidSystemState source can
                    // otherwise carry modifiers left over from a preceding chord, and
                    // cmd+Return inserts nothing (seen live after `key cmd+down`).
                    for down in [true, false] {
                        let event = CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: down)
                        event?.flags = []
                        event?.post(tap: .cghidEventTap)
                    }
                    usleep(2_000)
                    continue
                }
                let units = Array(String(character).utf16)
                for down in [true, false] {
                    let event = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                    event?.flags = []
                    event?.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                    event?.post(tap: .cghidEventTap)
                }
                usleep(2_000)
            }
        case nil:
            throw AgentError.invalidArgument(field: "input.action", reason: "missing or unknown to this agent")
        }
    }

    private static func point(_ point: Doppel_Common_V1_Point, field: String) throws -> CGPoint {
        let bounds = CGDisplayBounds(CGMainDisplayID())
        let p = CGPoint(x: point.x, y: point.y)
        guard bounds.contains(p) else {
            throw AgentError.invalidArgument(field: field, reason: "(\(point.x), \(point.y)) is outside the \(Int(bounds.width))×\(Int(bounds.height)) pt display")
        }
        return p
    }

    private static func mouse(_ type: CGEventType, at point: CGPoint, button: CGMouseButton, clickState: Int = 0,
                              flags: CGEventFlags = [], source: CGEventSource?) {
        guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: button) else { return }
        if clickState > 0 { event.setIntegerValueField(.mouseEventClickState, value: Int64(clickState)) }
        // Only the modifiers asked for: a click is a plain click (see the chord note above).
        event.flags = flags
        event.post(tap: .cghidEventTap)
    }
}
