// Reading and writing files inside the guest, as the logged-in user.
//
// Writes are atomic: the data goes to a temporary file in the same directory,
// gets its mode, and is renamed over the target, so a reader sees the old file
// or the new one, never a partial write. CREATE_NEW renames with RENAME_EXCL,
// so a file that appears in the meantime is never replaced.

import Foundation
import DoppelWire

enum GuestFiles {
    /// Per call, either way: matches the host's MAX_FILE_BYTES.
    static let maxBytes: UInt32 = 32 << 20

    static func read(_ request: Doppel_Agent_V1_ReadFile) throws -> Doppel_Agent_V1_FileContents {
        let path = try checked(request.path)
        let limit = request.maxBytes == 0 ? maxBytes : min(request.maxBytes, maxBytes)
        let fd = open(path, O_RDONLY | O_CLOEXEC)
        guard fd >= 0 else { throw failure(errno, "can't open \(path)") }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        var info = stat()
        guard fstat(fd, &info) == 0 else { throw failure(errno, "can't stat \(path)") }
        guard info.st_mode & S_IFMT == S_IFREG else {
            throw AgentError.invalidArgument(field: "path", reason: "\(path) is not a regular file")
        }
        guard info.st_size <= Int64(limit) else {
            throw AgentError.invalidArgument(field: "max_bytes", reason: "\(path) is \(info.st_size) bytes, over the \(limit)-byte limit")
        }
        // Read one byte past the limit: a file that grew since fstat is refused, not cut.
        let data = try handle.read(upToCount: Int(limit) + 1) ?? Data()
        guard data.count <= Int(limit) else {
            throw AgentError.invalidArgument(field: "max_bytes", reason: "\(path) grew past the \(limit)-byte limit while reading")
        }
        return .with { $0.data = data }
    }

    static func write(_ request: Doppel_Agent_V1_WriteFile) throws {
        let path = try checked(request.path)
        guard request.data.count <= Int(maxBytes) else {
            throw AgentError.invalidArgument(field: "data", reason: "over the \(maxBytes)-byte limit")
        }
        let mode = request.mode == 0 ? 0o644 : request.mode
        guard mode & ~0o777 == 0 else {
            throw AgentError.invalidArgument(field: "mode", reason: "only permission bits (0o777) are allowed")
        }
        let exclusive: Bool
        switch request.policy {
        case .createNew: exclusive = true
        case .replace: exclusive = false
        case .unspecified, .UNRECOGNIZED:
            throw AgentError.invalidArgument(field: "policy", reason: "must be CREATE_NEW or REPLACE")
        }
        let target = URL(fileURLWithPath: path)
        let directory = target.deletingLastPathComponent().path
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw AgentError.invalidArgument(field: "path", reason: "\(directory) is not an existing directory")
        }
        if exclusive, FileManager.default.fileExists(atPath: path) {
            throw AgentError.invalidState("\(path) already exists")
        }

        var template = Array("\(directory)/.\(target.lastPathComponent).doppel-XXXXXX".utf8CString)
        let fd = mkstemp(&template)
        guard fd >= 0 else { throw failure(errno, "can't create a temporary file in \(directory)") }
        let temporary = String(cString: template)
        var renamed = false
        defer { if !renamed { unlink(temporary) } }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        try handle.write(contentsOf: request.data)
        guard fchmod(fd, mode_t(mode)) == 0 else { throw failure(errno, "can't set the mode of \(path)") }
        guard fsync(fd) == 0 else { throw failure(errno, "can't flush \(path)") }
        try handle.close()

        let flags: UInt32 = exclusive ? UInt32(RENAME_EXCL) : 0
        guard renamex_np(temporary, path, flags) == 0 else {
            let code = errno
            if code == EEXIST { throw AgentError.invalidState("\(path) already exists") }
            throw failure(code, "can't move the new file into place at \(path)")
        }
        renamed = true
    }

    /// The host sends parsed paths; this is the agent's own check, not the parser.
    private static func checked(_ path: String) throws -> String {
        guard path.hasPrefix("/") else {
            throw AgentError.invalidArgument(field: "path", reason: "must be an absolute path")
        }
        guard !path.contains("\0") else {
            throw AgentError.invalidArgument(field: "path", reason: "contains NUL")
        }
        return path
    }

    private static func failure(_ code: Int32, _ what: String) -> AgentError {
        let reason = String(cString: strerror(code))
        switch code {
        case ENOENT, ENOTDIR, EISDIR: return .invalidArgument(field: "path", reason: "\(what): \(reason)")
        case EACCES, EPERM: return .invalidState("\(what): \(reason)")
        default: return .failed("\(what): \(reason)")
        }
    }
}
