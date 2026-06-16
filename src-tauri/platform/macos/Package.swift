// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NolandTunnelBridge",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "NolandTunnelBridge", targets: ["NolandTunnelBridge"]),
    ],
    targets: [
        .target(
            name: "Shared",
            path: "Shared"
        ),
        .executableTarget(
            name: "NolandTunnelBridge",
            dependencies: ["Shared"],
            path: "NolandTunnelBridge",
            linkerSettings: [
                .linkedFramework("Foundation"),
                .linkedFramework("NetworkExtension"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
    ]
)
