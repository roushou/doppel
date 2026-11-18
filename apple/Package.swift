// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Doppel",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "DoppelWire", targets: ["DoppelWire"]),
        .executable(name: "doppel-vmm", targets: ["doppel-vmm"]),
        .executable(name: "doppel-agent", targets: ["doppel-agent"]),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-protobuf.git", from: "1.28.0"),
        .package(url: "https://github.com/apple/swift-argument-parser.git", from: "1.5.0"),
    ],
    targets: [
        // Generated types (from the shared /proto via a symlink) and the frame codec.
        .target(
            name: "DoppelWire",
            dependencies: [.product(name: "SwiftProtobuf", package: "swift-protobuf")],
            // SwiftPM warns that swift-protobuf-config.json is "unhandled". Expected:
            // the plugin requires it in the target and refuses it under `exclude:`.
            plugins: [.plugin(name: "SwiftProtobufPlugin", package: "swift-protobuf")]
        ),
        .testTarget(name: "DoppelWireTests", dependencies: ["DoppelWire"]),
        // US-layout virtual key codes, shared by host input and the guest agent.
        .target(name: "DoppelKeys", dependencies: ["DoppelWire"]),
        // Shared memory and atomics Swift can't reach (variadic shm_open).
        .target(name: "DoppelShm"),
        // One process per VM: owns the VZVirtualMachine and serves the vmm channel.
        // Must be signed with com.apple.security.virtualization (scripts/build.sh).
        .target(
            name: "DoppelVMM",
            dependencies: ["DoppelWire", "DoppelKeys", "DoppelShm",
                           .product(name: "ArgumentParser", package: "swift-argument-parser")],
            linkerSettings: [.linkedFramework("Virtualization"), .linkedFramework("AppKit"),
                            .linkedFramework("VideoToolbox"), .linkedFramework("CoreVideo")]
        ),
        .executableTarget(name: "doppel-vmm", dependencies: ["DoppelVMM"]),
        .testTarget(name: "DoppelVMMTests", dependencies: ["DoppelVMM"]),
        // In a GUI session: serves the agent channel on vsock in a VM's guest,
        // or on a unix socket on this Mac. Needs Accessibility (input) and
        // Screen Recording (capture) in that session.
        .target(
            name: "DoppelAgent",
            dependencies: ["DoppelWire", "DoppelKeys",
                           .product(name: "ArgumentParser", package: "swift-argument-parser")],
            linkerSettings: [.linkedFramework("AppKit"), .linkedFramework("ScreenCaptureKit")]
        ),
        .executableTarget(name: "doppel-agent", dependencies: ["DoppelAgent"]),
        .testTarget(name: "DoppelAgentTests", dependencies: ["DoppelAgent", "DoppelKeys"]),
    ]
)
