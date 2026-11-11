// swift-tools-version:5.9
// AunCore: Swift bindings for aun-core. For every vX.Y.Z tag the release workflow makes a
// release-only commit (not on any branch) that sets `releaseVersion` and `releaseChecksum`, and
// tags it X.Y.Z for SwiftPM. Depend on a version (`from: "X.Y.Z"`), not on main: main's values
// point at an older release. See docs/RELEASING.md.
// Local development: run scripts/build-xcframework.sh, then build with AUN_LOCAL_XCFRAMEWORK=1.
import Foundation
import PackageDescription

let releaseVersion = "0.3.0-rc.1"
let releaseChecksum = "a5a80d132f7a499d6f2ad2372e058bb0b80240f879efb785b93f3cab404fc4e9"

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
