// Where the agent listens for doppeld: a vsock port inside a VM's guest, or
// a unix socket when the agent runs on the Mac doppeld runs on.

import ArgumentParser
import Darwin
import Foundation

/// Where the agent listens. A value of this type names a place the agent
/// can bind: a unix socket's path is absolute and fits `sockaddr_un`.
public enum Listener: Sendable, Equatable, CustomStringConvertible {
    /// A vsock port in a VM's guest. doppeld asks the VMM to connect here.
    case vsock(port: UInt32)
    /// A unix socket on this Mac, which only this user can connect to.
    case unix(UnixSocketPath)

    /// The agent's well-known vsock port.
    public static let defaultPort: UInt32 = 7400

    public var description: String {
        switch self {
        case .vsock(let port): "vsock port \(port)"
        case .unix(let path): "unix socket \(path)"
        }
    }

    /// The listener the command line names: `--port N` or `--socket PATH`,
    /// and the default vsock port when it names neither. A request for
    /// help is thrown too; `exit(withError:)` prints it.
    public static func parse(arguments: [String]) throws -> Listener {
        try AgentArguments.parse(arguments).listener
    }

    /// Prints what parsing threw (help, or an error with the usage) and
    /// exits: 0 for help, 64 for a usage error.
    public static func exit(withError error: any Error) -> Never {
        AgentArguments.exit(withError: error)
    }
}

/// doppel-agent's command line.
struct AgentArguments: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "doppel-agent",
        abstract: "Serves the agent channel in a GUI session."
    )

    @Option(help: "Listen on this vsock port, in a VM's guest (default: \(Listener.defaultPort)).")
    var port: UInt32?
    @Option(help: "Listen on a unix socket at this absolute path, on this Mac.", transform: { try UnixSocketPath($0) })
    var socket: UnixSocketPath?

    func validate() throws {
        guard port == nil || socket == nil else { throw ValidationError("give one of --port and --socket") }
    }

    var listener: Listener {
        switch (port, socket) {
        case (_, let path?): .unix(path)
        case (let port?, nil): .vsock(port: port)
        case (nil, nil): .vsock(port: Listener.defaultPort)
        }
    }
}

/// A socket path in a fresh directory beside another, that only this user
/// can enter: where a socket is bound before it is moved into place.
struct StagedSocket {
    let directory: String
    let socket: UnixSocketPath

    init(beside path: UnixSocketPath) throws(AgentServerError) {
        let parent = (path.path as NSString).deletingLastPathComponent
        var template = Array("\(parent)/.doppel-XXXXXX".utf8CString)
        // mkdtemp makes it 0700.
        guard mkdtemp(&template) != nil else { throw .system("mkdtemp in \(parent)", errno) }
        let directory = String(decoding: template.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
        guard let socket = try? UnixSocketPath("\(directory)/s") else {
            rmdir(directory)
            throw .noRoomToStage(path)
        }
        self.directory = directory
        self.socket = socket
    }

    /// Removes what's left: the socket, unless it was moved, and the directory.
    func remove() {
        unlink(socket.path)
        rmdir(directory)
    }
}

/// An absolute path short enough for a unix socket's address.
public struct UnixSocketPath: Sendable, Equatable, CustomStringConvertible {
    /// The longest path `sockaddr_un` holds, without its terminating NUL.
    public static let maxBytes = MemoryLayout.size(ofValue: sockaddr_un().sun_path) - 1

    public let path: String

    public init(_ path: String) throws(UnixSocketPathError) {
        guard path.hasPrefix("/") else { throw .relative(path) }
        guard path.utf8.count <= Self.maxBytes else { throw .tooLong(path, max: Self.maxBytes) }
        guard !path.utf8.contains(0) else { throw .containsNul }
        self.path = path
    }

    public var description: String { path }

    /// The address to bind or connect to.
    var address: sockaddr_un {
        var address = sockaddr_un()
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        address.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: Array(path.utf8)) }
        return address
    }
}

public enum UnixSocketPathError: Error, Equatable, CustomStringConvertible {
    case relative(String)
    case tooLong(String, max: Int)
    case containsNul

    public var description: String {
        switch self {
        case .relative(let path): "\(path) is relative: a socket path must be absolute"
        case .tooLong(let path, let max): "\(path) is longer than a unix socket allows (\(max) bytes)"
        case .containsNul: "a socket path can't contain a NUL byte"
        }
    }
}

extension Listener {
    /// A listening socket bound here.
    func bind() throws(AgentServerError) -> Int32 {
        switch self {
        case .vsock(let port): try Self.bindVsock(port: port)
        case .unix(let path): try Self.bindUnix(path)
        }
    }

    private static func bindVsock(port: UInt32) throws(AgentServerError) -> Int32 {
        let listener = socket(AF_VSOCK, SOCK_STREAM, 0)
        guard listener >= 0 else { throw .system("socket(AF_VSOCK)", errno) }
        var address = sockaddr_vm()
        address.svm_len = UInt8(MemoryLayout<sockaddr_vm>.size)
        address.svm_family = sa_family_t(AF_VSOCK)
        address.svm_port = port
        address.svm_cid = UInt32(VMADDR_CID_ANY)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_vm>.size)) }
        }
        return try listening(listener, bound: bound)
    }

    /// Binds a socket only this user can connect to. A socket left at the
    /// path by an agent that exited is replaced; a live one, or anything
    /// that isn't a socket, is refused and left alone.
    ///
    /// The socket is bound in a directory beside the path that only this
    /// user can enter, made owner-only there, then renamed into place: it
    /// is never reachable by others, and the process's umask, which every
    /// thread shares, is left alone. (`fchmod` before `bind` would be
    /// simpler, but macOS refuses it on sockets.)
    private static func bindUnix(_ path: UnixSocketPath) throws(AgentServerError) -> Int32 {
        var existing = stat()
        if lstat(path.path, &existing) == 0 {
            guard existing.st_mode & S_IFMT == S_IFSOCK else { throw .notASocket(path) }
            var address = path.address
            guard !isServed(&address) else { throw .alreadyServed(path) }
        }
        let staged = try StagedSocket(beside: path)
        defer { staged.remove() }
        let listener = socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw .system("socket(AF_UNIX)", errno) }
        var address = staged.socket.address
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0 else { let code = errno; close(listener); throw .system("bind", code) }
        guard chmod(staged.socket.path, 0o600) == 0 else { let code = errno; close(listener); throw .system("chmod \(staged.socket)", code) }
        // Replaces a socket an exited agent left, in one step.
        guard rename(staged.socket.path, path.path) == 0 else { let code = errno; close(listener); throw .system("rename to \(path)", code) }
        return try listening(listener, bound: 0)
    }

    /// Whether something accepts connections at this address now.
    private static func isServed(_ address: inout sockaddr_un) -> Bool {
        let probe = socket(AF_UNIX, SOCK_STREAM, 0)
        guard probe >= 0 else { return false }
        defer { close(probe) }
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(probe, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        return connected == 0
    }

    private static func listening(_ listener: Int32, bound: Int32) throws(AgentServerError) -> Int32 {
        guard bound == 0 else { let code = errno; close(listener); throw .system("bind", code) }
        guard Darwin.listen(listener, 8) == 0 else { let code = errno; close(listener); throw .system("listen", code) }
        return listener
    }

    /// Whether a connection accepted here may be served. A unix socket's
    /// peer must be this user: the socket's mode says so too, and this
    /// holds even if the mode is changed after the agent starts.
    func admits(_ connection: Int32) -> Bool {
        switch self {
        case .vsock: return true
        case .unix:
            var uid: uid_t = 0
            var gid: gid_t = 0
            return getpeereid(connection, &uid, &gid) == 0 && uid == geteuid()
        }
    }
}
