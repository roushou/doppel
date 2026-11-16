// Accessibility inside the guest: a raw, flat AX tree and actions on its nodes.
//
// The agent stays thin: one batched AXUIElementCopyMultipleAttributeValues
// call per node, pre-order, within the request's budgets. Compaction, refs and
// waiting are core logic on the host. The agent keeps the elements of its last
// few snapshots so the host can act on a node by (snapshot_id, node_index), and
// re-checks each element before acting on it: an element that is gone or whose
// role changed is a typed StaleReference, never an action on the wrong thing.

import AppKit
import ApplicationServices
import DoppelWire

@MainActor
final class AxSnapshotter {
    /// Snapshots whose elements are kept for actions. Older ones are stale.
    static let retained = 8
    /// Long text values (a whole document in a text view) are cut to this many characters.
    static let maxValueCharacters = 10_000
    /// Seconds the agent waits for an unresponsive app per AX call.
    static let messagingTimeout: Float = 1.0

    private struct Retained {
        let id: UInt64
        let elements: [AXUIElement]
        let roles: [String]
    }

    private var snapshots: [Retained] = []

    /// Set on the system-wide element, the timeout applies to every element,
    /// including the children a walk reaches through AXChildren. Set on the root
    /// alone, children kept the 6 s default: a busy app (Electron while it
    /// builds its tree) stalled the agent's main thread, and every other tool
    /// with it, for 15 s and more.
    init() {
        AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), Self.messagingTimeout)
    }

    /// Apps that accepted AXManualAccessibility in this agent process.
    private var exposed: Set<pid_t> = []
    /// After an app first accepts AXManualAccessibility it builds its tree
    /// asynchronously (VS Code: about 5 s). The first walk waits, up to this
    /// long, for its content root (AXWebArea) to appear.
    static let exposeWait: DispatchTimeInterval = .seconds(10)
    // A random start keeps ids from one agent process from matching another's after a restart.
    private var nextID = UInt64.random(in: 1 ..< (1 << 48))

    private static let attributes = [
        "AXRole", "AXSubrole", "AXTitle", "AXDescription", "AXIdentifier", "AXValue",
        "AXPosition", "AXSize", "AXEnabled", "AXFocused", "AXSelected", "AXExpanded", "AXChildren",
    ] as CFArray

    // MARK: Snapshot

    func snapshot(_ request: Doppel_Agent_V1_TakeAxSnapshot) throws -> Doppel_Agent_V1_AxSnapshot {
        guard AXIsProcessTrusted() else { throw AgentError.permissionMissing(.accessibility) }
        let (app, _) = try Self.root(for: request)
        if exposeTree(of: app.pid), Self.isElectron(app.pid) {
            let until = DispatchTime.now() + Self.exposeWait
            while DispatchTime.now() < until {
                // Resolved each time: the window element may change as the tree appears.
                if Self.hasWebArea(try Self.root(for: request).1) { break }
                usleep(250_000)
            }
        }
        let (_, root) = try Self.root(for: request)
        AXUIElementSetMessagingTimeout(root, Self.messagingTimeout)
        let excludeMenus = request.menus != .include
        let maxNodes = request.maxNodes == 0 ? 5_000 : Int(request.maxNodes)
        let maxDepth = request.maxDepth == 0 ? 64 : Int(request.maxDepth)
        let deadline = DispatchTime.now() + .milliseconds(request.deadlineMs == 0 ? 3_000 : Int(request.deadlineMs))

        var nodes: [Doppel_Agent_V1_AxNode] = []
        var elements: [AXUIElement] = []
        var roles: [String] = []
        var truncation: Doppel_Agent_V1_Truncation = .complete
        // (element, parent index, depth); popped from the end, so children are pushed reversed.
        var stack: [(AXUIElement, UInt32?, Int)] = [(root, nil, 0)]
        while let (element, parent, depth) = stack.popLast() {
            if nodes.count >= maxNodes { truncation = .nodeBudget; break }
            if DispatchTime.now() > deadline { truncation = .deadline; break }
            let (node, children) = Self.read(element, parent: parent)
            if excludeMenus, parent != nil, node.role == "AXMenuBar" || node.role == "AXMenu" { continue }
            let index = UInt32(nodes.count)
            nodes.append(node)
            elements.append(element)
            roles.append(node.role)
            guard !children.isEmpty else { continue }
            guard depth + 1 < maxDepth else {
                if truncation == .complete { truncation = .depthBudget }
                continue
            }
            for child in children.reversed() { stack.append((child, index, depth + 1)) }
        }

        let id = nextID
        nextID += 1
        // A person's look keeps nothing: the snapshots an agent acts on stay.
        if request.retention != .observation {
            snapshots.append(Retained(id: id, elements: elements, roles: roles))
            if snapshots.count > Self.retained { snapshots.removeFirst(snapshots.count - Self.retained) }
        }
        return .with {
            $0.snapshotID = id
            $0.app = app
            $0.nodes = nodes
            $0.truncation = truncation
        }
    }

    /// Electron apps (and recent Chromium) expose only a window frame until an
    /// assistive client asks for their tree with AXManualAccessibility. Native
    /// apps reject the attribute as unsupported, which is harmless. Not
    /// AXEnhancedUserInterface: it changes app behavior (window animations,
    /// positioning). Returns whether the app accepted it for the first time.
    private func exposeTree(of pid: pid_t) -> Bool {
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(app, Self.messagingTimeout)
        let result = AXUIElementSetAttributeValue(app, "AXManualAccessibility" as CFString, kCFBooleanTrue)
        guard result == .success else { return false }
        return exposed.insert(pid).inserted
    }

    /// Electron builds its tree only once asked, so the first walk waits for it.
    /// Chrome accepts AXManualAccessibility too but exposes page content only
    /// when launched with --force-renderer-accessibility: waiting would only cost
    /// 10 s. Electron apps ship their framework inside the bundle.
    private static func isElectron(_ pid: pid_t) -> Bool {
        guard let bundle = NSRunningApplication(processIdentifier: pid)?.bundleURL else { return false }
        let framework = bundle.appendingPathComponent("Contents/Frameworks/Electron Framework.framework")
        return FileManager.default.fileExists(atPath: framework.path)
    }

    /// Whether Chromium content is exposed under `root`: a bounded look, not a walk.
    private static func hasWebArea(_ root: AXUIElement) -> Bool {
        var queue = [(root, 0)]
        var seen = 0
        while !queue.isEmpty, seen < 200 {
            let (element, depth) = queue.removeFirst()
            seen += 1
            if copyValue(element, "AXRole") as? String == "AXWebArea" { return true }
            if depth < 6 { queue += elements(copyValue(element, "AXChildren")).map { ($0, depth + 1) } }
        }
        return false
    }

    private static func root(for request: Doppel_Agent_V1_TakeAxSnapshot) throws -> (Doppel_Agent_V1_RunningApp, AXUIElement) {
        switch request.scope {
        case .focusedWindow, nil:
            guard let running = NSWorkspace.shared.frontmostApplication else {
                throw AgentError.invalidState("no frontmost app")
            }
            let app = AXUIElementCreateApplication(running.processIdentifier)
            AXUIElementSetMessagingTimeout(app, messagingTimeout)
            for attribute in ["AXFocusedWindow", "AXMainWindow"] {
                if let window = element(copyValue(app, attribute)) { return (describe(running), window) }
            }
            if let first = elements(copyValue(app, "AXWindows")).first {
                return (describe(running), first)
            }
            throw AgentError.invalidState("\(running.localizedName ?? "the frontmost app") has no window; use the app scope")
        case .applicationPid(let pid):
            guard let running = NSRunningApplication(processIdentifier: pid) else {
                throw AgentError.invalidArgument(field: "application_pid", reason: "no running app with pid \(pid)")
            }
            return (describe(running), AXUIElementCreateApplication(pid))
        }
    }

    private static func describe(_ app: NSRunningApplication) -> Doppel_Agent_V1_RunningApp {
        .with {
            $0.pid = app.processIdentifier
            $0.bundleID = app.bundleIdentifier ?? ""
            $0.name = app.localizedName ?? ""
        }
    }

    /// One batched IPC call for the attributes, one for the action names.
    private static func read(_ element: AXUIElement, parent: UInt32?) -> (Doppel_Agent_V1_AxNode, [AXUIElement]) {
        var raw: CFArray?
        let status = AXUIElementCopyMultipleAttributeValues(element, attributes, [], &raw)
        var values: [CFTypeRef?] = Array(repeating: nil, count: CFArrayGetCount(attributes))
        if status == .success, let raw, CFGetTypeID(raw) == CFArrayGetTypeID() {
            for i in 0 ..< min(CFArrayGetCount(raw), values.count) {
                guard let item = CFArrayGetValueAtIndex(raw, i) else { continue }
                values[i] = present(Unmanaged<AnyObject>.fromOpaque(item).takeUnretainedValue())
            }
        }
        func value(_ i: Int) -> CFTypeRef? { i < values.count ? values[i] : nil }

        var node = Doppel_Agent_V1_AxNode()
        if let parent { node.parent = parent }
        node.role = value(0) as? String ?? ""
        node.subrole = value(1) as? String ?? ""
        node.title = value(2) as? String ?? ""
        node.description_p = value(3) as? String ?? ""
        node.identifier = value(4) as? String ?? ""
        if let v = axValue(value(5)) { node.value = v }
        if let origin = point(value(6)), let size = size(value(7)) {
            node.frame = .with {
                $0.origin = .with { $0.x = origin.x; $0.y = origin.y }
                $0.size = .with { $0.width = size.width; $0.height = size.height }
            }
        }
        node.flags = .with {
            if let b = value(8) as? Bool { $0.enabled = b }
            if let b = value(9) as? Bool { $0.focused = b }
            if let b = value(10) as? Bool { $0.selected = b }
            if let b = value(11) as? Bool { $0.expanded = b }
        }
        if node.role != "AXStaticText" {
            var names: CFArray?
            if AXUIElementCopyActionNames(element, &names) == .success, let names = names as? [String] {
                node.actions = names
            }
        }
        let children = elements(value(12))
        return (node, children)
    }

    /// Missing attributes come back as an AXValue of type axError.
    private static func present(_ value: AnyObject) -> CFTypeRef? {
        if CFGetTypeID(value) == AXValueGetTypeID(), AXValueGetType(value as! AXValue) == .axError { return nil }
        return value
    }

    private static func axValue(_ value: CFTypeRef?) -> Doppel_Agent_V1_AxValue? {
        guard let value else { return nil }
        if let text = value as? String {
            return .with { $0.text = String(text.prefix(maxValueCharacters)) }
        }
        if let text = value as? NSAttributedString {
            return .with { $0.text = String(text.string.prefix(maxValueCharacters)) }
        }
        if CFGetTypeID(value) == CFBooleanGetTypeID(), let flag = value as? Bool {
            return .with { $0.boolean = flag }
        }
        if let number = value as? NSNumber { return .with { $0.number = number.doubleValue } }
        return nil
    }

    private static func point(_ value: CFTypeRef?) -> CGPoint? {
        guard let value, CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        return AXValueGetValue(value as! AXValue, .cgPoint, &point) ? point : nil
    }

    private static func size(_ value: CFTypeRef?) -> CGSize? {
        guard let value, CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
        var size = CGSize.zero
        return AXValueGetValue(value as! AXValue, .cgSize, &size) ? size : nil
    }

    // CF values must be checked by type ID: Swift's `as?` to a CF type such as
    // AXUIElement always succeeds, whatever the object is, and an unchecked
    // non-element in an AXChildren array crashed the agent (bus error in read).

    private static func element(_ value: CFTypeRef?) -> AXUIElement? {
        guard let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return unsafeBitCast(value, to: AXUIElement.self)
    }

    /// The AXUIElements in a CFArray value; anything else in it is skipped.
    private static func elements(_ value: CFTypeRef?) -> [AXUIElement] {
        guard let value, CFGetTypeID(value) == CFArrayGetTypeID() else { return [] }
        let array = unsafeBitCast(value, to: CFArray.self)
        return (0 ..< CFArrayGetCount(array)).compactMap { i in
            guard let raw = CFArrayGetValueAtIndex(array, i) else { return nil }
            return element(Unmanaged<AnyObject>.fromOpaque(raw).takeUnretainedValue())
        }
    }

    private static func copyValue(_ element: AXUIElement, _ attribute: String) -> CFTypeRef? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else { return nil }
        return value
    }

    private static func copy<T>(_ element: AXUIElement, _ attribute: String) -> T? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else { return nil }
        return value as? T
    }

    // MARK: Actions

    /// Whether every ancestor up to the application still lists the element
    /// among its children. A parent may skip one transparent wrapper: Chromium's
    /// window group lists the web area, not the scroll area whose AXParent it
    /// is (seen live: every Electron action was refused as stale). An element of
    /// a dismissed sheet still fails: its window lists neither the sheet nor
    /// anything in it.
    private static func isAttached(_ element: AXUIElement) -> Bool {
        var current = element
        var below: AXUIElement?
        for _ in 0 ..< 256 {
            guard let role: String = copy(current, "AXRole") else { return false }
            if role == "AXApplication" { return true }
            guard let parent = Self.element(copyValue(current, "AXParent")) else { return false }
            let children = Self.elements(copyValue(parent, "AXChildren"))
            let lists = { (element: AXUIElement) in children.contains(where: { CFEqual($0, element) }) }
            guard lists(current) || below.map(lists) == true else { return false }
            below = current
            current = parent
        }
        return false
    }

    /// The table or outline row `element` sits in, a few levels up at most;
    /// `nil` for a row itself or an element outside one.
    static func row(holding element: AXUIElement) -> AXUIElement? {
        var current = element
        for _ in 0..<3 {
            guard let parent = Self.element(copyValue(current, "AXParent")) else { return nil }
            var role: CFTypeRef?
            guard AXUIElementCopyAttributeValue(parent, "AXRole" as CFString, &role) == .success else { return nil }
            if role as? String == "AXRow" { return parent }
            current = parent
        }
        return nil
    }

    func perform(_ request: Doppel_Agent_V1_PerformAxAction) throws {
        let stale = AgentError.staleReference(snapshotID: request.snapshotID, nodeIndex: request.nodeIndex)
        guard let snapshot = snapshots.last(where: { $0.id == request.snapshotID }) else { throw stale }
        let index = Int(request.nodeIndex)
        guard index < snapshot.elements.count else {
            throw AgentError.invalidArgument(field: "node_index", reason: "snapshot has \(snapshot.elements.count) nodes")
        }
        guard AXIsProcessTrusted() else { throw AgentError.permissionMissing(.accessibility) }
        let element = snapshot.elements[index]
        AXUIElementSetMessagingTimeout(element, Self.messagingTimeout)
        // Still there, and still the same kind of element? AX keeps answering
        // for elements of a closed sheet or window (seen live: "press" on the
        // Save button of a dismissed sheet reported success), so the role alone
        // isn't enough: the element must still hang off the app.
        var role: CFTypeRef?
        let read = AXUIElementCopyAttributeValue(element, "AXRole" as CFString, &role)
        guard read == .success, role as? String == snapshot.roles[index], Self.isAttached(element) else { throw stale }

        let what: String
        let result: AXError
        switch request.action {
        case .press:
            (what, result) = ("press", AXUIElementPerformAction(element, "AXPress" as CFString))
        case .showMenu:
            (what, result) = ("show_menu", AXUIElementPerformAction(element, "AXShowMenu" as CFString))
        case .raise:
            (what, result) = ("raise", AXUIElementPerformAction(element, "AXRaise" as CFString))
        case .named(let name):
            (what, result) = (name, AXUIElementPerformAction(element, name as CFString))
        case .focus:
            (what, result) = ("focus", AXUIElementSetAttributeValue(element, "AXFocused" as CFString, kCFBooleanTrue))
        case .select:
            // Selecting a row's cell selects the row: a list's cell (a
            // file's name in Finder) takes AXSelected without selecting
            // anything, so a model that picks the cell it read would
            // otherwise be told it worked.
            (what, result) = ("select", AXUIElementSetAttributeValue(Self.row(holding: element) ?? element, "AXSelected" as CFString, kCFBooleanTrue))
        case .setValue(let text):
            (what, result) = ("set_value", AXUIElementSetAttributeValue(element, "AXValue" as CFString, text as CFString))
        case nil:
            throw AgentError.invalidArgument(field: "action", reason: "missing or unknown to this agent")
        }
        switch result {
        case .success: return
        case .invalidUIElement: throw stale
        case .actionUnsupported, .attributeUnsupported, .notImplemented:
            throw AgentError.unsupported("\(what) on this \(snapshot.roles[index])")
        case .apiDisabled: throw AgentError.permissionMissing(.accessibility)
        case .notEnoughPrecision, .notificationAlreadyRegistered, .notificationNotRegistered,
             .notificationUnsupported, .noValue, .parameterizedAttributeUnsupported, .illegalArgument,
             .cannotComplete, .failure:
            throw AgentError.failed("\(what) failed: AXError \(result.rawValue)")
        @unknown default:
            throw AgentError.failed("\(what) failed: AXError \(result.rawValue)")
        }
    }
}
