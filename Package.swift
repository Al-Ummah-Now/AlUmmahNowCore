// swift-tools-version:5.9
// AunCore: Swift bindings for aun-core. The release workflow rewrites `releaseVersion` and
// `releaseChecksum` for every vX.Y.Z tag and tags that commit X.Y.Z for SwiftPM.
// Local development: run scripts/build-xcframework.sh, then build with AUN_LOCAL_XCFRAMEWORK=1.
import Foundation
import PackageDescription

let releaseVersion = "0.1.0"
let releaseChecksum = "dda913b90a260a60a2a330cb90765f57c18e13ce87cf4c4b7445366d96478c71"

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
            linkerSettings: [.linkedFramework("SystemConfiguration"), .linkedFramework("Security")]
        ),
        .testTarget(name: "AunCoreTests", dependencies: ["AunCore"], path: "apple/Tests/AunCoreTests"),
    ]
)
