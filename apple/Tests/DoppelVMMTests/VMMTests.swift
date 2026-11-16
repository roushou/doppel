import Foundation
import Testing
import AppKit
import Virtualization
import DoppelWire
@testable import DoppelVMM

@Suite struct CommandParsing {
    let files = ["--spec", "/m/spec.pb", "--disk", "/m/disk.img", "--aux", "/m/aux.bin"]

    @Test func serveWithStateAndWindow() throws {
        let command = try Command(arguments: ["serve"] + files + ["--socket", "/tmp/d/m.sock", "--consume-state", "/m/s.vzsave", "--window"])
        guard case .serve(let f, let socket, .consumeState(let state), .shown) = command else { Issue.record("\(command)"); return }
        #expect(f.disk.path == "/m/disk.img" && socket == "/tmp/d/m.sock" && state.path == "/m/s.vzsave")
    }

    @Test func coldBootHiddenByDefault() throws {
        guard case .serve(_, _, .coldBoot, .hidden) = try Command(arguments: ["serve"] + files + ["--socket", "/s"]) else {
            Issue.record("expected cold boot, hidden"); return
        }
    }

    @Test func provisionsTheFirstBoot() throws {
        let provision = ["--provision-user", "doppel", "--provision-full-name", "Doppel", "--provision-password-file", "/m/login-password"]
        guard case .serve(_, _, .provision(let account), .hidden) = try Command(arguments: ["serve"] + files + ["--socket", "/s"] + provision) else {
            Issue.record("expected a provisioning boot"); return
        }
        #expect(account == Provisioning(fullName: "Doppel", username: "doppel", passwordFile: URL(fileURLWithPath: "/m/login-password")))
    }

    @Test(arguments: [
        // Provisioning is all or nothing, and never a resume.
        ["serve", "--spec", "/a", "--disk", "/d", "--aux", "/a", "--socket", "/s", "--provision-user", "u"],
        ["serve", "--spec", "/a", "--disk", "/d", "--aux", "/a", "--socket", "/s", "--provision-user", "u",
         "--provision-full-name", "U", "--provision-password-file", "/p", "--consume-state", "/c"],
        ["serve", "--spec", "/a", "--disk", "/d", "--aux", "/a", "--socket", "/s", "--provision-user", "u",
         "--provision-full-name", "U", "--provision-password-file", "rel"],
        ["serve", "--spec", "/a"],                                            // missing files
        ["serve", "--spec", "rel", "--disk", "/d", "--aux", "/a", "--socket", "/s"], // relative path
        ["serve", "--spec", "/a", "--disk", "/d", "--aux", "/a", "--socket", "/s", "--bogus", "x"],
        ["serve", "--spec", "/a", "--spec", "/b"],
        ["frobnicate"],
        [],
    ])
    func rejectsBadArguments(arguments: [String]) {
        #expect(throws: UsageError.self) { try Command(arguments: arguments) }
    }
}

@Suite @MainActor struct ConfigurationChecks {
    let files = MachineFiles(spec: URL(fileURLWithPath: "/nonexistent/spec.pb"),
                             disk: URL(fileURLWithPath: "/nonexistent/disk.img"),
                             aux: URL(fileURLWithPath: "/nonexistent/aux.bin"))

    @Test func uninstalledSpecIsRefused() {
        #expect(throws: ConfigurationError.notInstalled) { try VZVirtualMachineConfiguration.doppel(spec: .init(), files: files) }
    }

    @Test func garbageHardwareModelIsRefused() {
        var spec = Doppel_Bundle_V1_MachineSpec()
        spec.hardwareModel = Data([1, 2, 3])
        spec.machineIdentifier = Data([4, 5, 6])
        #expect(throws: ConfigurationError.invalidHardwareModel) { try VZVirtualMachineConfiguration.doppel(spec: spec, files: files) }
    }
}

@Suite struct TextRecognition {
    /// White 2x image of a 400×200 pt display with black text drawn at a
    /// point given top-left: what a Retina capture of the guest looks like.
    static func image(_ text: String, at origin: CGPoint) throws -> CGImage {
        let scale: CGFloat = 2, points = CGSize(width: 400, height: 200)
        let context = try #require(CGContext(
            data: nil, width: Int(points.width * scale), height: Int(points.height * scale), bitsPerComponent: 8,
            bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(.white)
        context.fill(CGRect(x: 0, y: 0, width: points.width * scale, height: points.height * scale))
        context.scaleBy(x: scale, y: scale)
        NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
        let string = NSAttributedString(string: text, attributes: [.font: NSFont.systemFont(ofSize: 20), .foregroundColor: NSColor.black])
        string.draw(at: CGPoint(x: origin.x, y: points.height - origin.y - string.size().height))
        NSGraphicsContext.current = nil
        return try #require(context.makeImage())
    }

    @Test func linesAndWordsComeBackInDisplayPoints() throws {
        let image = try Self.image("Cancel   Save Changes", at: CGPoint(x: 40, y: 120))
        let text = try TextRecognizer.recognize(image, displayPoints: CGSize(width: 400, height: 200))
        #expect(text.displayPoints.width == 400 && text.displayPoints.height == 200)
        let line = try #require(text.lines.first)
        #expect(text.lines.count == 1)
        #expect(line.text.contains("Save Changes"), "\(line.text)")
        #expect(line.confidence > 0.3)
        // Top-left origin, in points: the line sits where it was drawn.
        #expect(abs(line.frame.origin.x - 40) < 8, "\(line.frame)")
        #expect(abs(line.frame.origin.y - 120) < 10, "\(line.frame)")
        #expect(line.frame.size.height > 10 && line.frame.size.height < 40, "\(line.frame)")
        #expect(line.words.map(\.text) == ["Cancel", "Save", "Changes"])
        let cancel = line.words[0].frame, save = line.words[1].frame
        #expect(cancel.origin.x + cancel.size.width <= save.origin.x + 1, "words in order, side by side")
        #expect(save.origin.x > 80 && save.origin.x < 200, "\(save)")
    }

    @Test func aBlankDisplayHasNoLines() throws {
        let text = try TextRecognizer.recognize(try Self.image("", at: .zero), displayPoints: CGSize(width: 400, height: 200))
        #expect(text.lines.isEmpty)
    }
}
