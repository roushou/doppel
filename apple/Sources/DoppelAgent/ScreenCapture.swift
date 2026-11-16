// Screen capture inside the guest through ScreenCaptureKit (needs Screen Recording).

import CoreGraphics
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers
import DoppelWire

enum ScreenCapture {
    @MainActor
    static func capture(_ request: Doppel_Agent_V1_CaptureScreen) async throws -> Doppel_Common_V1_Image {
        guard CGPreflightScreenCaptureAccess() else { throw AgentError.permissionMissing(.screenRecording) }
        let type: UTType
        switch request.encoding {
        case .png, .unspecified: type = .png
        case .jpeg: type = .jpeg
        case .UNRECOGNIZED(let raw): throw AgentError.invalidArgument(field: "encoding", reason: "unknown encoding \(raw)")
        }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let main = CGMainDisplayID()
        guard let display = content.displays.first(where: { $0.displayID == main }) ?? content.displays.first else {
            throw AgentError.failed("no display to capture")
        }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let points = CGSize(width: display.width, height: display.height)
        let scale = CGFloat(filter.pointPixelScale)
        var pixels = CGSize(width: points.width * scale, height: points.height * scale)
        let longest = max(pixels.width, pixels.height)
        if request.maxDimensionPixels > 0, longest > CGFloat(request.maxDimensionPixels) {
            let factor = CGFloat(request.maxDimensionPixels) / longest
            pixels = CGSize(width: (pixels.width * factor).rounded(), height: (pixels.height * factor).rounded())
        }
        let configuration = SCStreamConfiguration()
        configuration.width = Int(pixels.width)
        configuration.height = Int(pixels.height)
        configuration.showsCursor = true
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)

        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil) else {
            throw AgentError.failed("cannot create \(type.identifier) encoder")
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw AgentError.failed("encoding the capture failed") }
        return .with {
            $0.encoding = type == .png ? .png : .jpeg
            $0.widthPixels = UInt32(image.width)
            $0.heightPixels = UInt32(image.height)
            $0.displayPoints = .with { $0.width = points.width; $0.height = points.height }
            $0.data = data as Data
        }
    }
}
