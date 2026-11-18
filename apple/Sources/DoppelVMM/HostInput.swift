// Host-side input and capture through VZVirtualMachineView.
//
// Works before login and without a guest agent: events go to the VM's
// virtual keyboard and pointing devices, and pixels come from the view.
// Coordinates are guest display points, top-left origin.

import AppKit
import Virtualization
import DoppelWire
import DoppelKeys

enum HostInputError: Error, CustomStringConvertible {
    case missingAction
    case unsupportedKey(String)
    case unsupportedCharacter(Character)
    case outsideDisplay(CGPoint)
    case captureFailed
    case invalidPointer(String)

    var description: String {
        switch self {
        case .missingAction: "input has no action"
        case .unsupportedKey(let key): "key \(key) can't be sent through the host keyboard"
        case .unsupportedCharacter(let c): "character \(c) isn't on the host keyboard map (US layout); use the guest agent to type it"
        case .outsideDisplay(let p): "point (\(p.x), \(p.y)) is outside the guest display"
        case .captureFailed: "could not capture the VM display"
        case .invalidPointer(let reason): "invalid pointer event: \(reason)"
        }
    }
}

extension MachineRuntime {
    func send(_ input: Doppel_Common_V1_Input) throws {
        switch input.action {
        case .click(let click):
            let point = try viewPoint(click.at)
            let (down, up): (NSEvent.EventType, NSEvent.EventType) = switch click.button {
            case .right: (.rightMouseDown, .rightMouseUp)
            case .middle: (.otherMouseDown, .otherMouseUp)
            case .left, .unspecified, .UNRECOGNIZED: (.leftMouseDown, .leftMouseUp)
            }
            for n in 1...max(1, Int(click.count)) {
                mouse(down, at: point, clicks: n)
                mouse(up, at: point, clicks: n)
            }
        case .moveTo(let at):
            mouse(.mouseMoved, at: try viewPoint(at), clicks: 0)
        case .pointer(let pointer):
            let action: PointerAction
            do { action = try PointerAction(pointer) } catch let invalid as PointerAction.Invalid {
                throw HostInputError.invalidPointer(invalid.description)
            }
            let point = try viewPoint(pointer.at)
            var modifiers = Set<ModifierKey>()
            for modifier in pointer.modifiers {
                guard let key = ModifierKey(modifier) else { throw HostInputError.unsupportedKey("modifier \(modifier)") }
                modifiers.insert(key)
            }
            let (type, clicks): (NSEvent.EventType, Int) = switch action {
            case .press(.left, let n): (.leftMouseDown, n)
            case .press(.right, let n): (.rightMouseDown, n)
            case .press(.middle, let n): (.otherMouseDown, n)
            case .release(.left, let n): (.leftMouseUp, n)
            case .release(.right, let n): (.rightMouseUp, n)
            case .release(.middle, let n): (.otherMouseUp, n)
            case .move(held: nil): (.mouseMoved, 0)
            case .move(held: .left): (.leftMouseDragged, 1)
            case .move(held: .right): (.rightMouseDragged, 1)
            case .move(held: .middle): (.otherMouseDragged, 1)
            }
            // The guest's keyboard holds modifier state: press them around
            // the pointer event, as a key chord does.
            let hold = ModifierHold(modifiers)
            flags(hold.press)
            if !hold.press.isEmpty { usleep(Self.deviceSettle) }
            mouse(type, at: point, clicks: clicks, flags: hold.flags)
            if !hold.release.isEmpty { usleep(Self.deviceSettle) }
            flags(hold.release)
        case .drag(let drag):
            let from = try viewPoint(drag.from), to = try viewPoint(drag.to)
            mouse(.leftMouseDown, at: from, clicks: 1)
            let steps = max(2, min(30, Int(hypot(to.x - from.x, to.y - from.y) / 12)))
            for step in 1...steps {
                let t = CGFloat(step) / CGFloat(steps)
                mouse(.leftMouseDragged, at: CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t), clicks: 1)
            }
            mouse(.leftMouseUp, at: to, clicks: 1)
        case .scroll(let scroll):
            mouse(.mouseMoved, at: try viewPoint(scroll.at), clicks: 0)
            // Input's dy and dx are content motion (positive: towards the
            // top and the left), which the agent posts as is. Here they go
            // through the VM's pointing device, and the guest turns wheel
            // motion into content motion with its own natural-scrolling
            // setting, which the VMM can't read: send the wheel motion that
            // gives the asked-for content motion under macOS's default
            // (natural scrolling on). A guest with it off scrolls the other way.
            if let cg = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                                wheel1: Int32(-scroll.dy), wheel2: Int32(-scroll.dx), wheel3: 0),
               let event = NSEvent(cgEvent: cg) {
                view.scrollWheel(with: event)
            }
        case .key(let chord):
            var modifiers = Set<ModifierKey>()
            for modifier in chord.modifiers {
                guard let key = ModifierKey(modifier) else { throw HostInputError.unsupportedKey("modifier \(modifier)") }
                modifiers.insert(key)
            }
            let code: UInt16
            var characters = ""
            switch chord.key.key {
            case .named(let named):
                guard let key = VirtualKey(named: named) else { throw HostInputError.unsupportedKey("\(named)") }
                code = key.code
            case .character(let text):
                guard text.count == 1, let c = text.first, let key = VirtualKey(character: c) else { throw HostInputError.unsupportedKey(text) }
                code = key.code
                if key.needsShift { modifiers.insert(.shift) }
                characters = text
            case .virtualKeyCode(let raw):
                code = UInt16(truncatingIfNeeded: raw)
            case nil:
                throw HostInputError.missingAction
            }
            key(KeyStroke(code: code, modifiers: modifiers), characters: characters)
        case .typeText(let typing):
            // Validate everything first: never type half a string.
            let keys = try typing.text.map { c -> (Character, VirtualKey) in
                guard let key = VirtualKey(character: c) else { throw HostInputError.unsupportedCharacter(c) }
                return (c, key)
            }
            for (c, virtualKey) in keys {
                key(KeyStroke(code: virtualKey.code, modifiers: virtualKey.needsShift ? [.shift] : []), characters: String(c))
            }
        case nil:
            throw HostInputError.missingAction
        }
    }

    /// The view's pixels at full resolution.
    func displayPixels() throws -> NSBitmapImageRep {
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { throw HostInputError.captureFailed }
        view.cacheDisplay(in: view.bounds, to: rep)
        return rep
    }

    func captureDisplay(maxDimension: Int) throws -> Doppel_Common_V1_Image {
        let rep = try displayPixels()
        var image: NSBitmapImageRep = rep
        let longest = max(rep.pixelsWide, rep.pixelsHigh)
        if maxDimension > 0, longest > maxDimension, let cg = rep.cgImage {
            let scale = CGFloat(maxDimension) / CGFloat(longest)
            let width = Int(CGFloat(rep.pixelsWide) * scale), height = Int(CGFloat(rep.pixelsHigh) * scale)
            guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                          space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { throw HostInputError.captureFailed }
            context.interpolationQuality = .high
            context.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
            guard let scaled = context.makeImage() else { throw HostInputError.captureFailed }
            image = NSBitmapImageRep(cgImage: scaled)
        }
        guard let png = image.representation(using: .png, properties: [:]) else { throw HostInputError.captureFailed }
        return .with {
            $0.encoding = .png
            $0.widthPixels = UInt32(image.pixelsWide)
            $0.heightPixels = UInt32(image.pixelsHigh)
            $0.displayPoints = .with { $0.width = displayPoints.width; $0.height = displayPoints.height }
            $0.data = png
        }
    }

    /// The display's colour at `at`, as a screenshot would show it: the
    /// view's pixels there, in sRGB.
    func sampleDisplay(at point: Doppel_Common_V1_Point) throws -> Doppel_Common_V1_Color {
        guard point.x >= 0, point.y >= 0, point.x < displayPoints.width, point.y < displayPoints.height else {
            throw HostInputError.outsideDisplay(CGPoint(x: point.x, y: point.y))
        }
        let bounds = view.bounds
        let x = point.x * bounds.width / displayPoints.width
        let y = bounds.height - point.y * bounds.height / displayPoints.height
        let spot = NSRect(x: x.rounded(.down), y: (y - 1).rounded(.down), width: 1, height: 1).intersection(bounds)
        guard !spot.isEmpty, let rep = view.bitmapImageRepForCachingDisplay(in: spot) else { throw HostInputError.captureFailed }
        view.cacheDisplay(in: spot, to: rep)
        guard let color = rep.colorAt(x: 0, y: 0)?.usingColorSpace(.sRGB) else { throw HostInputError.captureFailed }
        let channel = { (value: CGFloat) in UInt32((min(max(value, 0), 1) * 255).rounded()) }
        return .with {
            $0.red = channel(color.redComponent)
            $0.green = channel(color.greenComponent)
            $0.blue = channel(color.blueComponent)
        }
    }

    /// Guest display points → view coordinates (bottom-left origin).
    private func viewPoint(_ point: Doppel_Common_V1_Point) throws -> NSPoint {
        guard point.x >= 0, point.y >= 0, point.x < displayPoints.width, point.y < displayPoints.height else {
            throw HostInputError.outsideDisplay(CGPoint(x: point.x, y: point.y))
        }
        let bounds = view.bounds
        let x = point.x * bounds.width / displayPoints.width
        let y = bounds.height - point.y * bounds.height / displayPoints.height
        return view.convert(NSPoint(x: x, y: y), to: nil)
    }

    private func mouse(_ type: NSEvent.EventType, at location: NSPoint, clicks: Int, flags: UInt = 0) {
        guard let event = NSEvent.mouseEvent(with: type, location: location, modifierFlags: NSEvent.ModifierFlags(rawValue: flags),
                                             timestamp: ProcessInfo.processInfo.systemUptime,
                                             windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: clicks, pressure: 1)
        else { return }
        switch type {
        case .leftMouseDown: view.mouseDown(with: event)
        case .leftMouseUp: view.mouseUp(with: event)
        case .leftMouseDragged: view.mouseDragged(with: event)
        case .rightMouseDown: view.rightMouseDown(with: event)
        case .rightMouseUp: view.rightMouseUp(with: event)
        case .rightMouseDragged: view.rightMouseDragged(with: event)
        case .otherMouseDown: view.otherMouseDown(with: event)
        case .otherMouseUp: view.otherMouseUp(with: event)
        case .otherMouseDragged: view.otherMouseDragged(with: event)
        case .mouseMoved: view.mouseMoved(with: event)
        default: break
        }
    }

    /// Microseconds between a modifier change and a pointer event. The
    /// keyboard and the pointer are separate virtual devices: without a
    /// pause, the guest can see the click before the shift that goes with it.
    static let deviceSettle: useconds_t = 30_000

    /// Modifier key changes alone (see ModifierHold).
    private func flags(_ events: [KeyStroke.Event]) {
        for step in events {
            guard let event = NSEvent.keyEvent(with: .flagsChanged, location: .zero, modifierFlags: NSEvent.ModifierFlags(rawValue: step.flags),
                                               timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                                               context: nil, characters: "", charactersIgnoringModifiers: "", isARepeat: false,
                                               keyCode: step.keyCode)
            else { continue }
            view.flagsChanged(with: event)
        }
    }

    private func key(_ stroke: KeyStroke, characters: String) {
        for step in stroke.events {
            let modifiers = NSEvent.ModifierFlags(rawValue: step.flags)
            let (type, text): (NSEvent.EventType, String) = switch step.phase {
            case .flagsChanged: (.flagsChanged, "")
            case .keyDown: (.keyDown, characters)
            case .keyUp: (.keyUp, characters)
            }
            guard let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: modifiers, timestamp: ProcessInfo.processInfo.systemUptime,
                                               windowNumber: window.windowNumber, context: nil, characters: text,
                                               charactersIgnoringModifiers: text.lowercased(), isARepeat: false, keyCode: step.keyCode)
            else { continue }
            switch step.phase {
            case .flagsChanged: view.flagsChanged(with: event)
            case .keyDown: view.keyDown(with: event)
            case .keyUp: view.keyUp(with: event)
            }
        }
    }
}
