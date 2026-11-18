// `doppel-vmm latest-image`: the newest macOS restore image this Mac can
// install, as Apple's catalog names it. Prints one line, `BUILD URL`, for
// `doppel setup` to download; nothing is downloaded here.

import Foundation
import Virtualization

public struct LatestImage: Sendable, Equatable {
    public let build: String
    public let url: URL

    public init(build: String, url: URL) {
        self.build = build; self.url = url
    }

    /// What the caller parses.
    public var line: String { "\(build) \(url.absoluteString)" }

    public static func fetch() async throws -> LatestImage {
        let image = try await VZMacOSRestoreImage.latestSupported
        return LatestImage(build: image.buildVersion, url: image.url)
    }
}
