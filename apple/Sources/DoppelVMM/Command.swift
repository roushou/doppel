// Command-line parsing for doppel-vmm: argv becomes a typed command once.

import ArgumentParser
import Foundation

public enum Command: Sendable {
    case serve(MachineFiles, socket: String, boot: Boot, window: WindowMode)
    case install(MachineFiles, ipsw: URL)
    /// Burns a reel (the overlay) into a recorded video.
    case export(reel: URL, out: URL)
    /// Prints the newest restore image this Mac can install: its build and URL.
    case latestImage
}

extension Command {
    /// Parses argv (without the program name) once into a typed command.
    /// A request for help is thrown too; `exit(withError:)` prints it.
    public init(arguments: [String]) throws {
        var parsed = try VMMArguments.parseAsRoot(arguments)
        switch parsed {
        case let subcommand as any CommandArguments:
            self = try subcommand.command()
        case is VMMArguments:
            throw ValidationError("missing command")
        default:
            // ArgumentParser's own commands (help): running one throws the
            // request it stands for.
            try parsed.run()
            throw ValidationError("missing command")
        }
    }

    /// Prints what parsing threw (help, or an error with the usage) and
    /// exits: 0 for help, 64 for a usage error.
    public static func exit(withError error: any Error) -> Never {
        VMMArguments.exit(withError: error)
    }
}

/// A subcommand's arguments, which name one `Command`.
private protocol CommandArguments: ParsableCommand {
    func command() throws -> Command
}

/// doppel-vmm's command line. Parsing yields one of its subcommands.
struct VMMArguments: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "doppel-vmm",
        abstract: "Runs one macOS VM. Started by doppel, never by hand.",
        subcommands: [Serve.self, Install.self, Export.self, LatestImage.self]
    )

    /// The files that make up an installed machine.
    struct Files: ParsableArguments {
        @Option(help: "The machine's spec.", transform: absolutePath) var spec: URL
        @Option(help: "The machine's disk image.", transform: absolutePath) var disk: URL
        @Option(help: "The machine's auxiliary storage.", transform: absolutePath) var aux: URL

        var machineFiles: MachineFiles { MachineFiles(spec: spec, disk: disk, aux: aux) }
    }

    struct Serve: CommandArguments {
        static let configuration = CommandConfiguration(abstract: "Boots the machine and serves its control socket.")

        @OptionGroup var files: Files
        @Option(help: "Where to serve the control socket.", transform: absolutePath) var socket: URL
        @Option(help: "A saved state to resume from instead of booting.", transform: absolutePath) var consumeState: URL?
        @Flag(help: "Show the machine's display in a window.") var window = false
        @Option(help: "The account to create on first boot.") var provisionUser: String?
        @Option(help: "That account's full name.") var provisionFullName: String?
        @Option(help: "A file holding that account's password.", transform: absolutePath) var provisionPasswordFile: URL?

        func validate() throws { _ = try boot() }

        func command() throws -> Command {
            .serve(files.machineFiles, socket: socket.path, boot: try boot(), window: window ? .shown : .hidden)
        }

        private func boot() throws -> Boot {
            switch (consumeState, provisionUser, provisionFullName, provisionPasswordFile) {
            case (let state?, nil, nil, nil):
                .consumeState(state)
            case (nil, nil, nil, nil):
                .coldBoot
            case (nil, let user?, let fullName?, let passwordFile?):
                .provision(Provisioning(fullName: fullName, username: user, passwordFile: passwordFile))
            case (.some, _, _, _):
                throw ValidationError("--consume-state and --provision-* exclude each other")
            case (nil, _, _, _):
                throw ValidationError("--provision-user, --provision-full-name and --provision-password-file go together")
            }
        }
    }

    struct Install: CommandArguments {
        static let configuration = CommandConfiguration(abstract: "Installs macOS from a restore image onto the machine's files.")

        @OptionGroup var files: Files
        @Option(help: "The restore image (.ipsw).", transform: absolutePath) var ipsw: URL

        func command() -> Command { .install(files.machineFiles, ipsw: ipsw) }
    }

    struct Export: CommandArguments {
        static let configuration = CommandConfiguration(abstract: "Burns a reel's overlay into its recorded video.")

        @Option(help: "The reel to export.", transform: absolutePath) var reel: URL
        @Option(help: "Where to write the video.", transform: absolutePath) var out: URL

        func command() -> Command { .export(reel: reel, out: out) }
    }

    struct LatestImage: CommandArguments {
        static let configuration = CommandConfiguration(abstract: "Prints the newest restore image this Mac can install: its build and URL.")

        func command() -> Command { .latestImage }
    }
}

/// A path option's value, which must be absolute.
private func absolutePath(_ value: String) throws -> URL {
    guard value.hasPrefix("/") else { throw ValidationError("must be an absolute path") }
    return URL(fileURLWithPath: value)
}
