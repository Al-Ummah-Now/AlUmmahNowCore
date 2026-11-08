// swift-tools-version:5.9
// AunCore: Swift bindings for aun-core. For every vX.Y.Z tag the release workflow makes a
// release-only commit (not on any branch) that sets `releaseVersion` and `releaseChecksum`, and
// tags it X.Y.Z for SwiftPM. Depend on a version (`from: "X.Y.Z"`), not on main: main's values
// point at an older release. See docs/RELEASING.md.
// Local development: run scripts/build-xcframework.sh, then build with AUN_LOCAL_XCFRAMEWORK=1.
import Foundation
import PackageDescription

let releaseVersion = "0.2.0"
let releaseChecksum = "96ad76ffc81937ae3576118d3aa8720ace9b486b6b2cf6e880035c6947478405"

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
