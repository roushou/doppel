// Events the agent pushes to subscribed connections: the frontmost app
// changed, or its accessibility tree did. They are hints that the host should
// look again, so they are coalesced (one pending event per kind) and paced,
// and a connection that isn't reading never makes the agent buffer more.

import AppKit
import ApplicationServices
import DoppelWire

/// Fans events out to every subscribed connection.
public final class EventHub: @unchecked Sendable {
    private let lock = NSLock()
    private var outboxes: [ObjectIdentifier: EventOutbox] = [:]

    public init() {}

    func subscribe(_ outbox: EventOutbox) {
        lock.lock(); outboxes[ObjectIdentifier(outbox)] = outbox; lock.unlock()
    }

    func unsubscribe(_ outbox: EventOutbox) {
        lock.lock(); outboxes[ObjectIdentifier(outbox)] = nil; lock.unlock()
    }

    func publish(_ event: Doppel_Agent_V1_AgentEvent) {
        lock.lock(); let all = Array(outboxes.values); lock.unlock()
        for outbox in all { outbox.post(event) }
    }
}

/// One connection's undelivered events: at most one per kind, the latest.
final class EventOutbox: @unchecked Sendable {
    private let condition = NSCondition()
    private var pending: [Doppel_Agent_V1_AgentEvent] = []
    private var closed = false

    func post(_ event: Doppel_Agent_V1_AgentEvent) {
        condition.lock()
        defer { condition.unlock() }
        guard !closed else { return }
        if let same = pending.firstIndex(where: { Self.kind($0) == Self.kind(event) }) {
            pending[same] = event
        } else {
            pending.append(event)
        }
        condition.signal()
    }

    /// Blocks until there are events to send; nil once closed.
    func take() -> [Doppel_Agent_V1_AgentEvent]? {
        condition.lock()
        defer { condition.unlock() }
        while pending.isEmpty && !closed { condition.wait() }
        guard !closed else { return nil }
        defer { pending = [] }
        return pending
    }

    func close() {
        condition.lock(); closed = true; pending = []; condition.broadcast(); condition.unlock()
    }

    private static func kind(_ event: Doppel_Agent_V1_AgentEvent) -> Int {
        switch event.kind {
        case .permissionsChanged: 0
        case .frontmostAppChanged: 1
        case .uiChanged: 2
        case nil: 3
        }
    }
}

/// Watches the frontmost app and its accessibility tree, on the main run loop.
@MainActor
public final class UiWatcher {
    private let hub: EventHub
    private var observer: AXObserver?
    private var token: NSObjectProtocol?

    /// What counts as the UI changing. Registered on the app element, they
    /// cover every element of the app.
    private static let notifications = [
        kAXFocusedUIElementChangedNotification, kAXFocusedWindowChangedNotification,
        kAXMainWindowChangedNotification, kAXWindowCreatedNotification,
        kAXUIElementDestroyedNotification, kAXCreatedNotification,
        kAXValueChangedNotification, kAXTitleChangedNotification,
        kAXSelectedChildrenChangedNotification, kAXSelectedTextChangedNotification,
        kAXRowCountChangedNotification, kAXLayoutChangedNotification,
        kAXMenuOpenedNotification, kAXMenuClosedNotification, kAXSheetCreatedNotification,
    ]

    public init(hub: EventHub) {
        self.hub = hub
        token = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
        ) { [weak self] notification in
            let app = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
            MainActor.assumeIsolated { self?.frontmostChanged(to: app) }
        }
        frontmostChanged(to: NSWorkspace.shared.frontmostApplication, announce: false)
    }

    private func frontmostChanged(to app: NSRunningApplication?, announce: Bool = true) {
        if announce {
            hub.publish(.with {
                if let app {
                    $0.frontmostAppChanged.app = .with {
                        $0.pid = app.processIdentifier
                        $0.bundleID = app.bundleIdentifier ?? ""
                        $0.name = app.localizedName ?? ""
                    }
                } else {
                    $0.frontmostAppChanged = .init()
                }
            })
        }
        observe(app?.processIdentifier)
    }

    /// Moves the AX observer to `pid`. Without the Accessibility grant there
    /// is nothing to observe, and only frontmost changes are pushed.
    private func observe(_ pid: pid_t?) {
        if let observer {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .defaultMode)
            self.observer = nil
        }
        guard let pid, AXIsProcessTrusted() else { return }
        var created: AXObserver?
        let callback: AXObserverCallback = { _, element, _, refcon in
            guard let refcon else { return }
            let hub = Unmanaged<EventHub>.fromOpaque(refcon).takeUnretainedValue()
            var pid: pid_t = 0
            _ = AXUIElementGetPid(element, &pid)
            hub.publish(.with { $0.uiChanged.pid = pid })
        }
        guard AXObserverCreate(pid, callback, &created) == .success, let created else { return }
        let app = AXUIElementCreateApplication(pid)
        // The hub outlives every watcher: the agent keeps both for its lifetime.
        let refcon = Unmanaged.passUnretained(hub).toOpaque()
        for name in Self.notifications {
            _ = AXObserverAddNotification(created, app, name as CFString, refcon)
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(created), .defaultMode)
        observer = created
    }
}
