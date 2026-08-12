// swift-tools-version:5.9
import PackageDescription

// The computer-use native helper for the host broker. A standalone, signed
// executable the broker spawns once per operation to perform the parts of
// computer use that need macOS frameworks: screen capture (ScreenCaptureKit),
// accessibility-tree reads (AXUIElement), window enumeration (CGWindowList),
// and input synthesis (CGEvent). It is a dumb executor: the broker owns every
// capability check, consent gate, and audit entry, and only invokes this
// helper for an already-authorized operation.
//
// Kept outside the broker Cargo crate on purpose: it is a separate
// language/toolchain artifact with its own code signature, which the macOS TCC
// grants (Screen Recording, Accessibility) bind to. Keep the signing identity
// and the bundled path stable across releases — changing either resets the
// user's grants.
let package = Package(
    name: "tidebreak-cu-helper",
    platforms: [
        // SCScreenshotManager requires macOS 14. The Accessibility and
        // CGWindowList APIs are far older; 14 is the binding constraint.
        .macOS(.v14)
    ],
    targets: [
        .executableTarget(
            name: "tidebreak-cu-helper",
            path: "Sources/tidebreak-cu-helper"
        )
    ]
)
