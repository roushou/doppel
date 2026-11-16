// The guest display as a stream of frames in shared memory, as an encoded
// stream (see Streamer), and as a recording (see Recorder).
//
// VZVirtualMachineView keeps the guest framebuffer as an IOSurface in its
// layer, swapped or redrawn on every frame whether or not the window is
// shown. The share polls for a new surface, converts it to NV12
// on a queue of its own and writes it into a ring of slots that display
// clients map read-only (see DisplayLayout in vmm.proto).
//
// The layer's structure is VZ's, not API: if it stops holding an IOSurface,
// opening the display fails and existing clients stop receiving frames.

import AppKit
import CoreMedia
import CoreVideo
import DoppelShm
import DoppelWire
import Foundation
import IOSurface
import Virtualization
import VideoToolbox

public enum DisplayShareError: Error, CustomStringConvertible {
    case system(String, Int32)
    case conversion(String, OSStatus)

    public var description: String {
        switch self {
        case .system(let call, let code): "\(call): \(String(cString: strerror(code)))"
        case .conversion(let call, let status): "\(call) failed (\(status))"
        }
    }
}

@MainActor
final class DisplayShare {
    /// How often the layer is checked for a new frame: twice the fastest
    /// display rate, so no frame waits more than half a refresh.
    private static let poll: DispatchTimeInterval = .microseconds(8_333)

    private let view: VZVirtualMachineView
    private let server: ControlServer
    private let writer: FrameWriter
    private var timer: DispatchSourceTimer?
    private var shown: SurfaceVersion?
    /// Ticks in a row without a client. A new client's feed registers just
    /// after its reply, so polling stops only after a while without one.
    private var idleTicks = 0
    private static let idleTicksBeforeStopping = 120
    /// Mirrors whether the writer holds a recorder, for `tick`.
    private var recording = false
    /// An encoded connection needs a key frame, not yet encoded.
    private var keyFrameOwed = false

    var layout: Doppel_Vmm_V1_DisplayLayout { writer.layout }

    init(view: VZVirtualMachineView, server: ControlServer, pixels: (width: Int, height: Int), points: CGSize) throws {
        self.view = view
        self.server = server
        writer = try FrameWriter(width: pixels.width, height: pixels.height, points: points)
    }

    /// A descriptor for the shared memory, for a new client to map. The
    /// caller owns it.
    func descriptor() throws -> Int32 {
        let fd = dup(writer.fd)
        guard fd >= 0 else { throw DisplayShareError.system("dup", errno) }
        return fd
    }

    /// Starts polling, if it isn't already; it stops once no client is left.
    func start() {
        idleTicks = 0
        guard timer == nil else { return }
        shown = nil
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now(), repeating: Self.poll, leeway: .milliseconds(1))
        timer.setEventHandler { [weak self] in
            MainActor.assumeIsolated { self?.tick() }
        }
        self.timer = timer
        timer.resume()
    }

    private func tick() {
        let streaming = server.displayClients > 0
        let encoding = server.encodedClients > 0
        guard streaming || encoding || recording else {
            idleTicks += 1
            if idleTicks >= Self.idleTicksBeforeStopping {
                timer?.cancel()
                timer = nil
            }
            return
        }
        idleTicks = 0
        if server.takeKeyFrameRequest() {
            keyFrameOwed = true
            // The screen as it is now, even if it hasn't changed.
            shown = nil
        }
        // A display connection joined: it gets the screen as it is now,
        // though nothing changed while another client kept this running.
        if server.takeFrameRequest() { shown = nil }
        // The encoder dropped a frame: the screen as it is now, again.
        if writer.takeDropped() { shown = nil }
        guard let surface = currentSurface() else { return }
        let version = SurfaceVersion(surface)
        guard version != shown else { return }
        // The writer still busy with the last frame: try again next tick.
        guard writer.begin() else { return }
        shown = version
        let key = keyFrameOwed
        keyFrameOwed = false
        let writer = writer, server = server
        let source = UncheckedSurface(surface)
        let seen = CMClockGetTime(CMClockGetHostTimeClock())
        writer.queue.async {
            if streaming, let frame = writer.write(source.surface) { server.publish(frame) }
            if encoding {
                writer.streamer(publishingTo: server)?.encode(source.surface, at: seen, key: key)
            } else {
                // No viewer left: the encoder's memory goes back.
                writer.streamer = nil
            }
            writer.recorder?.append(source.surface, at: seen)
            writer.end()
        }
    }

    /// Starts recording to `path`, unless a recording is running already.
    func startRecording(path: String) throws -> Doppel_Vmm_V1_RecordingStarted {
        let writer = writer
        if let running = writer.queue.sync(execute: { writer.recorder?.url.path }) {
            return .with { $0.path = running; $0.start = .alreadyRecording }
        }
        guard path.hasPrefix("/") else { throw RecordingError.notAbsolute(path) }
        guard !FileManager.default.fileExists(atPath: path) else { throw RecordingError.exists(path) }
        let recorder = try Recorder(url: URL(fileURLWithPath: path), width: Int(layout.widthPixels),
                                    height: Int(layout.heightPixels), origin: CMClockGetTime(CMClockGetHostTimeClock()))
        writer.queue.sync { writer.recorder = recorder }
        recording = true
        // The screen as it is now is the recording's first frame.
        shown = nil
        start()
        return .with { $0.path = path; $0.start = .started }
    }

    /// Stops the recording to `path` (any, if nil) once its file is complete.
    func stopRecording(path: String?) async throws -> Doppel_Vmm_V1_RecordingStopped {
        let writer = writer
        // After this, no frame is appended: the queue ran every earlier one.
        let taken: Recorder? = writer.queue.sync {
            guard let recorder = writer.recorder, path == nil || recorder.url.path == path else { return nil }
            writer.recorder = nil
            return recorder
        }
        guard let taken else { throw RecordingError.notRecording(path) }
        recording = false
        return try await taken.finish(at: CMClockGetTime(CMClockGetHostTimeClock()))
    }

    /// The surface the view's layer shows now.
    private func currentSurface() -> IOSurfaceRef? {
        guard let root = view.layer else { return nil }
        var pending = [root]
        while let layer = pending.popLast() {
            if let contents = layer.contents, CFGetTypeID(contents as CFTypeRef) == IOSurfaceGetTypeID() {
                return unsafeDowncast(contents as AnyObject, to: IOSurfaceRef.self)
            }
            pending.append(contentsOf: layer.sublayers ?? [])
        }
        return nil
    }
}

/// A surface's identity and content version: either changes when VZ draws.
private struct SurfaceVersion: Equatable {
    let id: IOSurfaceID
    let seed: UInt32

    init(_ surface: IOSurfaceRef) {
        id = IOSurfaceGetID(surface)
        seed = IOSurfaceGetSeed(surface)
    }
}

/// IOSurface is thread-safe; the reference is only passed to the writer.
private struct UncheckedSurface: @unchecked Sendable {
    let surface: IOSurfaceRef
    init(_ surface: IOSurfaceRef) { self.surface = surface }
}

/// Owns the shared memory and writes frames into it. Used on `queue` only,
/// apart from `begin`, which the main actor calls.
private final class FrameWriter: @unchecked Sendable {
    static let slotCount = 3
    static let headerBytes = 64
    static let alignment = 64
    static let page = 16_384

    let queue = DispatchQueue(label: "doppel.display.writer", qos: .userInteractive)
    let layout: Doppel_Vmm_V1_DisplayLayout
    let fd: Int32
    private let memory: UnsafeMutableRawPointer
    private let length: Int
    private let session: VTPixelTransferSession
    /// The conversion's target; copied into a slot from there.
    private let scratch: CVPixelBuffer
    private var sequence: UInt64 = 0
    /// The running recording; touched on `queue` only.
    var recorder: Recorder?
    /// The encoder for encoded connections, while there are any; touched on
    /// `queue` only.
    var streamer: Streamer?
    private let busy = NSLock()
    private var writing = false
    /// The encoder dropped a frame since `takeDropped` last asked.
    private var droppedSince = false

    init(width: Int, height: Int, points: CGSize) throws {
        // NV12 needs even dimensions.
        let width = width & ~1, height = height & ~1
        let lumaRow = Self.align(width, Self.alignment)
        let chromaRow = Self.align(width, Self.alignment)
        let lumaOffset = Self.headerBytes
        let chromaOffset = Self.align(lumaOffset + lumaRow * height, Self.alignment)
        let stride = Self.align(chromaOffset + chromaRow * height / 2, Self.page)
        length = stride * Self.slotCount
        layout = .with {
            $0.widthPixels = UInt32(width)
            $0.heightPixels = UInt32(height)
            $0.displayPoints = .with { $0.width = points.width; $0.height = points.height }
            $0.slotCount = UInt32(Self.slotCount)
            $0.slotStride = UInt64(stride)
            $0.lumaOffset = UInt64(lumaOffset)
            $0.lumaBytesPerRow = UInt32(lumaRow)
            $0.chromaOffset = UInt64(chromaOffset)
            $0.chromaBytesPerRow = UInt32(chromaRow)
        }

        fd = doppel_shm_create(length)
        guard fd >= 0 else { throw DisplayShareError.system("shm_open", errno) }
        guard let mapped = mmap(nil, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0), mapped != MAP_FAILED else {
            let code = errno
            close(fd)
            throw DisplayShareError.system("mmap", code)
        }
        memory = mapped

        var created: VTPixelTransferSession?
        var status = VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &created)
        guard status == noErr, let created else {
            munmap(mapped, length); close(fd)
            throw DisplayShareError.conversion("VTPixelTransferSessionCreate", status)
        }
        session = created
        var buffer: CVPixelBuffer?
        let attributes = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary] as CFDictionary
        status = CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, attributes, &buffer)
        guard status == kCVReturnSuccess, let buffer else {
            munmap(mapped, length); close(fd)
            throw DisplayShareError.conversion("CVPixelBufferCreate", status)
        }
        scratch = buffer
    }

    deinit {
        munmap(memory, length)
        close(fd)
    }

    private static func align(_ value: Int, _ to: Int) -> Int { (value + to - 1) / to * to }

    /// Claims the writer for one frame; false while the last one is in progress.
    func begin() -> Bool {
        busy.withLock {
            guard !writing else { return false }
            writing = true
            return true
        }
    }

    func end() { busy.withLock { writing = false } }

    /// Whether the encoder dropped a frame since the last call.
    func takeDropped() -> Bool {
        busy.withLock {
            defer { droppedSince = false }
            return droppedSince
        }
    }

    /// The encoder, made on first use; nil, and logged, if it can't be.
    /// Its first frame is a key frame.
    func streamer(publishingTo server: ControlServer) -> Streamer? {
        if let streamer { return streamer }
        do {
            let made = try Streamer(width: Int(layout.widthPixels), height: Int(layout.heightPixels),
                                    dropped: { [weak self] in self?.busy.withLock { self?.droppedSince = true } }) {
                server.publishEncoded($0)
            }
            streamer = made
            return made
        } catch {
            log("the display can't be encoded: \(error)")
            return nil
        }
    }

    /// Converts `surface` and writes it to the next slot. Nil when the
    /// conversion fails: the frame is skipped.
    func write(_ surface: IOSurfaceRef) -> Doppel_Vmm_V1_DisplayFrame? {
        var source: Unmanaged<CVPixelBuffer>?
        guard CVPixelBufferCreateWithIOSurface(nil, surface, nil, &source) == kCVReturnSuccess,
              let source = source?.takeRetainedValue()
        else { return nil }
        guard VTPixelTransferSessionTransferImage(session, from: source, to: scratch) == noErr else { return nil }

        sequence += 1
        let slot = Int((sequence - 1) % UInt64(Self.slotCount))
        let base = memory + slot * Int(layout.slotStride)
        // 0 while writing: a reader that sees it, or a different sequence
        // after reading, drops what it read.
        doppel_store_release_u64(base, 0)
        guard CVPixelBufferLockBaseAddress(scratch, .readOnly) == kCVReturnSuccess else { return nil }
        copyPlane(0, to: base + Int(layout.lumaOffset), rowBytes: Int(layout.lumaBytesPerRow), rows: Int(layout.heightPixels))
        copyPlane(1, to: base + Int(layout.chromaOffset), rowBytes: Int(layout.chromaBytesPerRow), rows: Int(layout.heightPixels) / 2)
        CVPixelBufferUnlockBaseAddress(scratch, .readOnly)
        doppel_store_release_u64(base, sequence)
        return .with { $0.slot = UInt32(slot); $0.sequence = sequence }
    }

    private func copyPlane(_ plane: Int, to destination: UnsafeMutableRawPointer, rowBytes: Int, rows: Int) {
        guard let source = CVPixelBufferGetBaseAddressOfPlane(scratch, plane) else { return }
        let sourceRow = CVPixelBufferGetBytesPerRowOfPlane(scratch, plane)
        let width = min(rowBytes, sourceRow)
        if sourceRow == rowBytes {
            destination.copyMemory(from: source, byteCount: rowBytes * rows)
        } else {
            for row in 0..<rows {
                (destination + row * rowBytes).copyMemory(from: source + row * sourceRow, byteCount: width)
            }
        }
    }
}
