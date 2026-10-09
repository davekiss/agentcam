// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "agentcam",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "agentcam-mac", targets: ["agentcam-mac"]),
        .library(name: "RecKit", targets: ["RecKit"]),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.5.0"),
    ],
    targets: [
        .target(
            name: "RecKit",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .executableTarget(
            name: "agentcam-mac",
            dependencies: [
                "RecKit",
                .product(name: "ArgumentParser", package: "swift-argument-parser"),
            ],
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                // An unbundled CLI needs an embedded Info.plist for TCC to show camera/mic prompts.
                .unsafeFlags(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__info_plist", "-Xlinker", "Support/Info.plist"]),
            ]
        ),
        .testTarget(
            name: "RecKitTests",
            dependencies: ["RecKit"],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
