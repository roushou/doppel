// doppel-vmm: one process per VM. Started by `doppel`, never by hand.
// `doppel-vmm --help` lists its commands; Command.swift parses them.

import AppKit
import DoppelVMM

let command: Command
do {
    command = try Command(arguments: Array(CommandLine.arguments.dropFirst()))
} catch {
    Command.exit(withError: error)
}

_ = NSApplication.shared
var runtime: MachineRuntime?
Task { @MainActor in
    do {
        switch command {
        case .serve(let files, let socket, let boot, let window):
            let r = try MachineRuntime(files: files, socketPath: socket)
            runtime = r
            try await r.run(boot: boot, window: window)
        case .install(let files, let ipsw):
            await Installer(files: files, restoreImage: ipsw).run()
            exit(0)
        case .export(let reel, let out):
            let exported = try await Exporter.export(reel: try Exporter.reel(at: reel), to: out)
            print("exported \(exported.frames) frames (\(String(format: "%.1f", exported.seconds)) s)")
            exit(0)
        case .latestImage:
            let image = try await LatestImage.fetch()
            print(image.line)
            exit(0)
        }
    } catch {
        FileHandle.standardError.write(Data("doppel-vmm: \(error)\n".utf8))
        exit(1)
    }
}
NSApp.run()
