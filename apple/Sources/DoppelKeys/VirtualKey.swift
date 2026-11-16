// macOS virtual key codes for the US layout, shared by both input paths:
// host input in the VMM (NSEvent into VZVirtualMachineView) and guest input
// in the agent (CGEvent).

import DoppelWire

public struct VirtualKey: Sendable, Equatable {
    public let code: UInt16
    /// The character needs Shift held on a US keyboard.
    public let needsShift: Bool

    public init(code: UInt16, needsShift: Bool = false) {
        self.code = code; self.needsShift = needsShift
    }

    /// The key that types `character` on a US keyboard, if there is one.
    public init?(character: Character) {
        if character == "\n" { self.init(code: 36); return }
        guard let entry = Self.characters[character] else { return nil }
        self = entry
    }

    public init?(named: Doppel_Common_V1_NamedKey) {
        let code: UInt16
        switch named {
        case .return: code = 36
        case .tab: code = 48
        case .space: code = 49
        case .delete: code = 51
        case .escape: code = 53
        case .forwardDelete: code = 117
        case .left: code = 123
        case .right: code = 124
        case .down: code = 125
        case .up: code = 126
        case .home: code = 115
        case .end: code = 119
        case .pageUp: code = 116
        case .pageDown: code = 121
        case .f1: code = 122
        case .f2: code = 120
        case .f3: code = 99
        case .f4: code = 118
        case .f5: code = 96
        case .f6: code = 97
        case .f7: code = 98
        case .f8: code = 100
        case .f9: code = 101
        case .f10: code = 109
        case .f11: code = 103
        case .f12: code = 111
        case .unspecified, .UNRECOGNIZED: return nil
        }
        self.init(code: code)
    }

    private static let characters: [Character: VirtualKey] = {
        var map: [Character: VirtualKey] = [:]
        let plain: [(Character, UInt16)] = [
            ("a", 0), ("s", 1), ("d", 2), ("f", 3), ("h", 4), ("g", 5), ("z", 6), ("x", 7), ("c", 8), ("v", 9), ("b", 11),
            ("q", 12), ("w", 13), ("e", 14), ("r", 15), ("y", 16), ("t", 17), ("1", 18), ("2", 19), ("3", 20), ("4", 21),
            ("6", 22), ("5", 23), ("=", 24), ("9", 25), ("7", 26), ("-", 27), ("8", 28), ("0", 29), ("]", 30), ("o", 31),
            ("u", 32), ("[", 33), ("i", 34), ("p", 35), ("l", 37), ("j", 38), ("'", 39), ("k", 40), (";", 41), ("\\", 42),
            (",", 43), ("/", 44), ("n", 45), ("m", 46), (".", 47), (" ", 49), ("`", 50),
        ]
        for (c, code) in plain {
            map[c] = VirtualKey(code: code)
            if c.isLetter { map[Character(c.uppercased())] = VirtualKey(code: code, needsShift: true) }
        }
        let shifted: [(Character, Character)] = [
            ("!", "1"), ("@", "2"), ("#", "3"), ("$", "4"), ("%", "5"), ("^", "6"), ("&", "7"), ("*", "8"), ("(", "9"), (")", "0"),
            ("_", "-"), ("+", "="), ("{", "["), ("}", "]"), ("|", "\\"), (":", ";"), ("\"", "'"), ("<", ","), (">", "."), ("?", "/"), ("~", "`"),
        ]
        for (c, base) in shifted { if let key = map[base] { map[c] = VirtualKey(code: key.code, needsShift: true) } }
        return map
    }()
}

/// A modifier key as the host keyboard path presses it: its own key code, its
/// device-independent flag and the device-dependent bit for the left-hand key.
/// VZVirtualMachineView works out which key went down or up from a
/// flagsChanged event's key code and these bits, so each modifier needs its own.
public enum ModifierKey: CaseIterable, Sendable {
    case control, option, shift, command, function

    public init?(_ wire: Doppel_Common_V1_Modifier) {
        switch wire {
        case .control: self = .control
        case .option: self = .option
        case .shift: self = .shift
        case .command: self = .command
        case .function: self = .function
        case .unspecified, .UNRECOGNIZED: return nil
        }
    }

    public var keyCode: UInt16 {
        switch self {
        case .control: 59
        case .option: 58
        case .shift: 56
        case .command: 55
        case .function: 63
        }
    }

    /// The flags of the modifier whose key this is; 0 for any other key.
    public static func flags(ofKeyCode code: UInt16) -> UInt {
        allCases.first { $0.keyCode == code }?.flags ?? 0
    }

    /// NSEvent.ModifierFlags raw value, device-independent bit plus the
    /// left-hand device bit (NX_DEVICEL*KEYMASK). Fn has no device bit.
    /// CGEventFlags uses the same bit layout.
    public var flags: UInt {
        switch self {
        case .control: (1 << 18) | 0x0001
        case .option: (1 << 19) | 0x0020
        case .shift: (1 << 17) | 0x0002
        case .command: (1 << 20) | 0x0008
        case .function: 1 << 23
        }
    }
}

/// The host keyboard events for one key press with modifiers held: each
/// modifier pressed in turn, the key down and up with all of them held, then
/// the modifiers released in reverse. Pure, so it is testable without a VM.
public struct KeyStroke: Equatable, Sendable {
    public enum Phase: Equatable, Sendable { case flagsChanged, keyDown, keyUp }

    public struct Event: Equatable, Sendable {
        public let phase: Phase
        public let keyCode: UInt16
        /// NSEvent.ModifierFlags raw value in force for this event.
        public let flags: UInt

        public init(phase: Phase, keyCode: UInt16, flags: UInt) {
            self.phase = phase; self.keyCode = keyCode; self.flags = flags
        }
    }

    public let events: [Event]

    public init(code: UInt16, modifiers: Set<ModifierKey>) {
        let hold = ModifierHold(modifiers)
        events = hold.press
            + [Event(phase: .keyDown, keyCode: code, flags: hold.flags), Event(phase: .keyUp, keyCode: code, flags: hold.flags)]
            + hold.release
    }
}

/// Modifiers held around another event: each pressed in turn, then released
/// in reverse. What the host keyboard needs for a shortcut or a shift-click.
public struct ModifierHold: Equatable, Sendable {
    public let press: [KeyStroke.Event]
    public let release: [KeyStroke.Event]
    /// NSEvent.ModifierFlags raw value while all are held.
    public let flags: UInt

    public init(_ modifiers: Set<ModifierKey>) {
        let ordered = ModifierKey.allCases.filter(modifiers.contains)
        var press: [KeyStroke.Event] = [], release: [KeyStroke.Event] = []
        var held: UInt = 0
        for modifier in ordered {
            held |= modifier.flags
            press.append(KeyStroke.Event(phase: .flagsChanged, keyCode: modifier.keyCode, flags: held))
        }
        flags = held
        for modifier in ordered.reversed() {
            held &= ~modifier.flags
            release.append(KeyStroke.Event(phase: .flagsChanged, keyCode: modifier.keyCode, flags: held))
        }
        self.press = press
        self.release = release
    }
}

/// A pointer event as each side posts it: decoded once from the wire.
public enum PointerAction: Equatable, Sendable {
    public enum Button: Equatable, Sendable { case left, right, middle }

    case press(Button, clicks: Int)
    case release(Button, clicks: Int)
    /// With a button held, a drag.
    case move(held: Button?)

    public struct Invalid: Error, CustomStringConvertible {
        public let description: String
    }

    public init(_ pointer: Doppel_Common_V1_Pointer) throws {
        let button: Button? = switch pointer.button {
        case .left: .left
        case .right: .right
        case .middle: .middle
        case .unspecified: nil
        case .UNRECOGNIZED(let raw): throw Invalid(description: "unknown mouse button \(raw)")
        }
        let clicks = max(1, min(3, Int(pointer.clickCount)))
        switch pointer.phase {
        case .press:
            guard let button else { throw Invalid(description: "a press needs a button") }
            self = .press(button, clicks: clicks)
        case .release:
            guard let button else { throw Invalid(description: "a release needs a button") }
            self = .release(button, clicks: clicks)
        case .move:
            self = .move(held: button)
        case .unspecified, .UNRECOGNIZED:
            throw Invalid(description: "unknown pointer phase")
        }
    }
}
