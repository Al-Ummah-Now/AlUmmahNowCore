// swift-tools-version:5.9
// AunCore: Swift bindings for aun-core. For every vX.Y.Z tag the release workflow makes a
// release-only commit (not on any branch) that sets `releaseVersion` and `releaseChecksum`, and
// tags it X.Y.Z for SwiftPM. Depend on a version (`from: "X.Y.Z"`), not on main: main's values
// point at an older release. See docs/RELEASING.md.
// Local development: run scripts/build-xcframework.sh, then build with AUN_LOCAL_XCFRAMEWORK=1.
import Foundation
import PackageDescription

let releaseVersion = "0.4.0-rc.4"
let releaseChecksum = "65b9d609e071e6dbb00959e418e8c36ebf2d64526aa2d573d3b5fdf547f0c1af"

let useLocal = ProcessInfo.processInfo.environment["AUN_LOCAL_XCFRAMEWORK"] != nil
let ffiTarget: Target = useLocal
    ? .binaryTarget(name: "AunCoreFFI", path: "build/AunCoreFFI.xcframework")
    : .binaryTarget(
        name: "AunCoreFFI",
        url: "https://github.com/Al-Ummah-Now/AlUmmahNowCore/releases/download/v\(releaseVersion)/AunCoreFFI.xcframework.zip",
        checksum: releaseChecksum
    )

let package = Package(
    name: "AunCore",
    platforms: [.iOS(.v15), .macOS(.v12), .tvOS(.v15)],
    products: [
        .library(name: "AunCore", targets: ["AunCore"]),
    ],
    targets: [
        ffiTarget,
        .target(
            name: "AunCore",
            dependencies: ["AunCoreFFI"],
            path: "apple/Sources/AunCore",
            linkerSettings: [
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("Security"),
                // The macOS slice includes whisper.cpp (on-device transcription, D10)
                .linkedLibrary("c++", .when(platforms: [.macOS])),
                .linkedFramework("Accelerate", .when(platforms: [.macOS])),
            ]
        ),
        .testTarget(name: "AunCoreTests", dependencies: ["AunCore"], path: "apple/Tests/AunCoreTests"),
    ]
)
