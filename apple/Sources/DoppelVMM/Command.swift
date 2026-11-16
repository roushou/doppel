// Command-line parsing for doppel-vmm: argv becomes a typed command once.

import Foundation

public enum Command: Sendable {
    case serve(MachineFiles, socket: String, boot: Boot, window: WindowMode)
    case install(MachineFiles, ipsw: URL)
}

public enum UsageError: Error, Equatable, CustomStringConvertible {
    case usage(String)
    public var description: String {
        switch self { case .usage(let detail): "\(detail)\nusage: doppel-vmm serve|install --spec S --disk D --aux A ..." }
    }
}

extension Command {
    /// Parses argv once into a typed command.
    public init(arguments: [String]) throws {
        guard let verb = arguments.first else { throw UsageError.usage("missing command") }
        var options: [String: String] = [:]
        var flags: Set<String> = []
        var rest = arguments.dropFirst()
        while let argument = rest.popFirst() {
            guard argument.hasPrefix("--") else { throw UsageError.usage("unexpected \(argument)") }
            let name = String(argument.dropFirst(2))
            if name == "window" { flags.insert(name); continue }
            guard let value = rest.popFirst() else { throw UsageError.usage("\(argument) needs a value") }
            guard options.updateValue(value, forKey: name) == nil else { throw UsageError.usage("\(argument) given twice") }
        }
        func path(_ name: String) throws -> URL {
            guard let value = options.removeValue(forKey: name) else { throw UsageError.usage("missing --\(name)") }
            guard value.hasPrefix("/") else { throw UsageError.usage("--\(name) must be an absolute path") }
            return URL(fileURLWithPath: value)
        }
        let files = MachineFiles(spec: try path("spec"), disk: try path("disk"), aux: try path("aux"))
        switch verb {
        case "serve":
            let socket = try path("socket").path
            let provision = (
                options.removeValue(forKey: "provision-user"),
                options.removeValue(forKey: "provision-full-name"),
                options["provision-password-file"] != nil ? try path("provision-password-file") : nil
            )
            let boot: Boot
            switch (options["consume-state"] != nil, provision) {
            case (true, (nil, nil, nil)):
                boot = .consumeState(try path("consume-state"))
            case (false, (nil, nil, nil)):
                boot = .coldBoot
            case (false, (let user?, let fullName?, let passwordFile?)):
                boot = .provision(Provisioning(fullName: fullName, username: user, passwordFile: passwordFile))
            case (true, _):
                throw UsageError.usage("--consume-state and --provision-* exclude each other")
            case (false, _):
                throw UsageError.usage("--provision-user, --provision-full-name and --provision-password-file go together")
            }
            self = .serve(files, socket: socket, boot: boot, window: flags.contains("window") ? .shown : .hidden)
        case "install":
            self = .install(files, ipsw: try path("ipsw"))
        default:
            throw UsageError.usage("unknown command \(verb)")
        }
        guard options.isEmpty else { throw UsageError.usage("unknown option --\(options.keys.sorted()[0])") }
    }
}

