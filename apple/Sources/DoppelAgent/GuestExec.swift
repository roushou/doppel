// Running a program inside the guest session, as the logged-in user.
//
// No shell: the program runs directly with its arguments. Output is read as it
// arrives (so the program never blocks on a full pipe), kept up to the limit
// per stream, and the rest is drained and dropped. On timeout, or when the
// request is cancelled (the host hung up), the program gets SIGTERM, then
// SIGKILL a second later. Only the program itself is signalled: children it
// spawned in their own process groups may outlive it.
//
// posix_spawn and waitid, not Foundation's Process: in the agent, Process
// missed a program's exit until another program was spawned (seen live: a
// 2 s program's reply came 13 s late, and the host gave up). The kernel's own
// wait on a dedicated thread can't miss it.

import Darwin
import Foundation
import DoppelWire

enum GuestExec {
    static func run(_ request: Doppel_Agent_V1_Exec) async throws -> Doppel_Agent_V1_ExecResult {
        guard request.program.hasPrefix("/") else {
            throw AgentError.invalidArgument(field: "program", reason: "must be an absolute path")
        }
        let input = try Pipe(), output = try Pipe(), errors = try Pipe()
        let child: Child
        do {
            child = try Child.spawn(request, stdin: input.read, stdout: output.write, stderr: errors.write)
        } catch {
            for fd in [input.read, input.write, output.read, output.write, errors.read, errors.write] { close(fd) }
            throw error
        }
        // The child holds its own copies; the parent's must close for EOF to reach the readers.
        for fd in [input.read, output.write, errors.write] { close(fd) }

        let limit = Int(request.maxOutputBytes == 0 ? 64 * 1024 : request.maxOutputBytes)
        let stdout = Capture(limit: limit), stderr = Capture(limit: limit)
        let done = DispatchGroup()
        for (fd, capture) in [(output.read, stdout), (errors.read, stderr)] {
            done.enter()
            Thread.detachNewThread {
                capture.drain(fd)
                close(fd)
                done.leave()
            }
        }
        // A program may exit without reading its input: the write then fails with
        // EPIPE instead of raising SIGPIPE. The write end always closes, so the
        // program sees EOF.
        _ = fcntl(input.write, F_SETNOSIGPIPE, 1)
        let stdinData = request.stdin
        Thread.detachNewThread {
            stdinData.withUnsafeBytes { raw in
                var offset = 0
                while offset < raw.count, let base = raw.baseAddress {
                    let n = write(input.write, base + offset, raw.count - offset)
                    if n < 0 { if errno == EINTR { continue }; break }
                    offset += n
                }
            }
            close(input.write)
        }

        let timedOut = Flag()
        let timeout = DispatchTime.now() + .milliseconds(Int(request.timeoutMs == 0 ? 10_000 : request.timeoutMs))
        DispatchQueue.global().asyncAfter(deadline: timeout) {
            if child.stop() { timedOut.set() }
        }

        let ending = await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Child.Ending, Never>) in
                Thread.detachNewThread {
                    let ending = child.wait()
                    done.wait()
                    continuation.resume(returning: ending)
                }
            }
        } onCancel: {
            _ = child.stop()
        }

        return .with {
            switch (timedOut.isSet, ending) {
            case (true, _): $0.timedOut = .init()
            case (false, .exited(let code)): $0.exitCode = code
            case (false, .signaled(let signal)): $0.signal = signal
            }
            $0.stdout = stdout.data
            $0.stderr = stderr.data
            $0.completeness = stdout.truncated || stderr.truncated ? .truncated : .full
        }
    }

    private struct Pipe {
        let read: Int32
        let write: Int32

        init() throws {
            var fds: [Int32] = [0, 0]
            guard pipe(&fds) == 0 else { throw AgentError.failed("pipe: \(String(cString: strerror(errno)))") }
            (read, write) = (fds[0], fds[1])
        }
    }

    /// A spawned program. Signals reach it only until it is reaped, so a
    /// signal can never land on a later process that reused its pid.
    private final class Child: @unchecked Sendable {
        enum Ending { case exited(Int32), signaled(Int32) }

        private let pid: pid_t
        private let lock = NSLock()
        private var reaped = false

        private init(pid: pid_t) { self.pid = pid }

        static func spawn(_ request: Doppel_Agent_V1_Exec, stdin: Int32, stdout: Int32, stderr: Int32) throws -> Child {
            var actions: posix_spawn_file_actions_t?
            posix_spawn_file_actions_init(&actions)
            defer { posix_spawn_file_actions_destroy(&actions) }
            posix_spawn_file_actions_adddup2(&actions, stdin, 0)
            posix_spawn_file_actions_adddup2(&actions, stdout, 1)
            posix_spawn_file_actions_adddup2(&actions, stderr, 2)
            if !request.workingDirectory.isEmpty {
                posix_spawn_file_actions_addchdir_np(&actions, request.workingDirectory)
            }

            var attributes: posix_spawnattr_t?
            posix_spawnattr_init(&attributes)
            defer { posix_spawnattr_destroy(&attributes) }
            // Only 0–2 reach the program, and every signal starts at its default:
            // the agent ignores SIGPIPE, its programs must not.
            posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK))
            var all = sigset_t(), none = sigset_t()
            sigfillset(&all)
            sigemptyset(&none)
            posix_spawnattr_setsigdefault(&attributes, &all)
            posix_spawnattr_setsigmask(&attributes, &none)

            let argv: [UnsafeMutablePointer<CChar>?] = ([request.program] + request.arguments).map { strdup($0) } + [nil]
            defer { argv.forEach { free($0) } }
            var pid: pid_t = 0
            let status = posix_spawn(&pid, request.program, &actions, &attributes, argv, environ)
            guard status == 0 else {
                throw AgentError.invalidArgument(field: "program", reason: "can't run \(request.program): \(String(cString: strerror(status)))")
            }
            return Child(pid: pid)
        }

        /// Blocks until the program ends, then reaps it.
        func wait() -> Ending {
            var info = siginfo_t()
            // Wait without reaping, so `signal` can't race the pid's reuse.
            while waitid(P_PID, id_t(pid), &info, WEXITED | WNOWAIT) != 0 && errno == EINTR {}
            lock.lock()
            defer { lock.unlock() }
            var status: Int32 = 0
            while waitpid(pid, &status, 0) < 0 && errno == EINTR {}
            reaped = true
            // <sys/wait.h>'s WIFEXITED and friends aren't imported into Swift.
            let signal = status & 0x7f
            return signal == 0 ? .exited((status >> 8) & 0xff) : .signaled(signal)
        }

        /// SIGTERM, then SIGKILL a second later. Whether the program was
        /// still running to be stopped.
        func stop() -> Bool {
            guard signal(SIGTERM) else { return false }
            DispatchQueue.global().asyncAfter(deadline: .now() + 1) { _ = self.signal(SIGKILL) }
            return true
        }

        /// Whether the signal was sent: false once the program has ended.
        func signal(_ signal: Int32) -> Bool {
            lock.lock()
            defer { lock.unlock() }
            guard !reaped else { return false }
            var info = siginfo_t()
            // Exited but not yet reaped: it has ended, don't report a timeout.
            if waitid(P_PID, id_t(pid), &info, WEXITED | WNOWAIT | WNOHANG) == 0, info.si_pid == pid { return false }
            return kill(pid, signal) == 0
        }
    }

    /// One output stream, kept up to a limit.
    private final class Capture: @unchecked Sendable {
        private let limit: Int
        private let lock = NSLock()
        private var kept = Data()
        private var dropped = false

        init(limit: Int) { self.limit = limit }

        func drain(_ fd: Int32) {
            var buffer = [UInt8](repeating: 0, count: 64 * 1024)
            while true {
                let n = read(fd, &buffer, buffer.count)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { return }
                lock.lock()
                let room = limit - kept.count
                if room > 0 { kept.append(contentsOf: buffer[0 ..< min(n, room)]) }
                if n > room { dropped = true }
                lock.unlock()
            }
        }

        var data: Data { lock.lock(); defer { lock.unlock() }; return kept }
        var truncated: Bool { lock.lock(); defer { lock.unlock() }; return dropped }
    }

    private final class Flag: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false
        func set() { lock.lock(); value = true; lock.unlock() }
        var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
    }
}
