// The account macOS creates on a machine's first boot after an install,
// through VZMacGuestProvisioningOptions (macOS 27 hosts and guests).

import Foundation
import Virtualization

public enum ProvisioningError: Error, Equatable, CustomStringConvertible {
    case unreadablePassword(String)
    case emptyPassword
    case unsupportedHost

    public var description: String {
        switch self {
        case .unreadablePassword(let path): "can't read the provisioning password from \(path)"
        case .emptyPassword: "the provisioning password file is empty"
        case .unsupportedHost: "provisioning a guest account needs a macOS 27 host"
        }
    }
}

/// An account for the guest: logged in automatically, with Remote Login
/// (SSH) on, so the host can install the agent before any agent runs.
/// The password stays in its file until the VM starts; it's never in argv.
public struct Provisioning: Sendable, Equatable {
    public let fullName: String
    public let username: String
    public let passwordFile: URL

    public init(fullName: String, username: String, passwordFile: URL) {
        self.fullName = fullName; self.username = username; self.passwordFile = passwordFile
    }

    /// Reads the password: one line, its final newline dropped.
    func password() throws -> String {
        guard let data = FileManager.default.contents(atPath: passwordFile.path),
              let text = String(data: data, encoding: .utf8) else {
            throw ProvisioningError.unreadablePassword(passwordFile.path)
        }
        let line = text.hasSuffix("\n") ? String(text.dropLast()) : text
        guard !line.isEmpty else { throw ProvisioningError.emptyPassword }
        return line
    }

    @MainActor
    func startOptions() throws -> VZMacOSVirtualMachineStartOptions {
        guard #available(macOS 27, *) else { throw ProvisioningError.unsupportedHost }
        let account = VZMacGuestProvisioningOptions()
        account.fullName = fullName
        account.username = username
        account.password = try password()
        account.logsInAutomatically = true
        account.enablesRemoteLogin = true
        let options = VZMacOSVirtualMachineStartOptions()
        try options.setGuestProvisioning(account)
        return options
    }
}
