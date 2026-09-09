import CoreGraphics
import Darwin
import Foundation

/// CoreGraphics exports these functions but omits them from public headers.
/// Apple's WebKit test runner declares this ABI and sets local coordinates after
/// global coordinates. Keep lookup and verification here; never guess event fields.
/// Reference: WebKit 835c8016b492f302189935e14739b165149f6d3c,
/// Tools/TestRunnerShared/spi/CoreGraphicsTestSPI.h and
/// Tools/TestRunnerShared/EventSerialization/mac/EventSerializerMac.mm.
enum EventWindowLocation {
    private typealias Getter = @convention(c) (CGEvent) -> CGPoint
    private typealias Setter = @convention(c) (CGEvent, CGPoint) -> Void
    private static let handle = dlopen(
        "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_LAZY | RTLD_LOCAL)
    private static let getter: Getter? = {
        guard let handle, let address = dlsym(handle, "CGEventGetWindowLocation") else {
            return nil
        }
        return unsafeBitCast(address, to: Getter.self)
    }()
    private static let setter: Setter? = {
        guard let handle, let address = dlsym(handle, "CGEventSetWindowLocation") else {
            return nil
        }
        return unsafeBitCast(address, to: Setter.self)
    }()

    static func set(_ event: CGEvent, point: CGPoint) throws {
        guard point.x.isFinite, point.y.isFinite, let getter, let setter else {
            throw HelperError(
                code: .independentInputUnavailable,
                message: "this macOS version lacks independent window coordinates")
        }
        setter(event, point)
        let actual = getter(event)
        guard actual.x == point.x, actual.y == point.y else {
            throw HelperError(
                code: .independentInputUnavailable,
                message: "macOS did not retain independent window coordinates")
        }
    }

    static func get(_ event: CGEvent) -> CGPoint? { getter?(event) }
}
