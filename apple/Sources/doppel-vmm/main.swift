// doppel-vmm: one process per VM. Started by `doppel`, never by hand.
//
//   doppel-vmm serve   --spec S --disk D --aux A --socket P [--consume-state F] [--window]
//                      [--provision-user U --provision-full-name N --provision-password-file F]
//   doppel-vmm install --spec S --disk D --aux A --ipsw I
//   doppel-vmm export  --video V --reel R --out O

import AppKit
import DoppelVMM

let command: Command
do {
    command = try Command(arguments: Array(CommandLine.arguments.dropFirst()))
} catch {
    FileHandle.standardError.write(Data("doppel-vmm: \(error)\n".utf8))
    exit(64)
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
        case .export(let video, let reel, let out):
            let exported = try await Exporter.export(video: video, reel: try Exporter.reel(at: reel), to: out)
            print("exported \(exported.frames) frames (\(String(format: "%.1f", exported.seconds)) s)")
            exit(0)
        }
    } catch {
        FileHandle.standardError.write(Data("doppel-vmm: \(error)\n".utf8))
        exit(1)
    }
}
NSApp.run()
