import AppKit
import CoreGraphics
import Foundation
import ScreenCaptureKit

/// Screenshot capture via ScreenCaptureKit (macOS 14 `SCScreenshotManager`).
/// Writes a PNG to the broker-provided `out_path` and returns only metadata —
/// the broker never receives raw image bytes over stdout. It stages the file
/// and hands the host a reference; the host attaches it as multimodal input.
enum Capture {
    struct Result: Encodable {
        let width: Int
        let height: Int
        let path: String
        let mediaType: String
    }

    static func run(_ request: HelperRequest) async throws -> Result {
        // Surface both permission modals (Screen Recording + Accessibility) up
        // front so the user grants everything in one pass instead of hitting a
        // second modal and restart the first time a different tool runs.
        // Capture itself only needs Screen Recording, which activates on a
        // fresh process — so this invocation still fails until the user grants
        // it and relaunches.
        guard Permissions.requestAll().screenRecording else {
            throw HelperError(
                code: .permissionDenied, message: "Screen Recording permission is not granted")
        }
        guard let outPath = request.outPath, !outPath.isEmpty else {
            throw HelperError(code: .invalidRequest, message: "capture requires out_path")
        }
        guard let target = request.target else {
            throw HelperError(code: .invalidRequest, message: "capture requires target")
        }

        let content = try await SCShareableContent.current
        let (filter, size, display) = try buildFilter(target, request: request, content: content)

        let config = SCStreamConfiguration()
        config.width = size.width
        config.height = size.height
        // Capture the full pixel buffer; keep the cursor out of analytical
        // screenshots.
        config.showsCursor = false

        let image = try await SCScreenshotManager.captureImage(
            contentFilter: filter, configuration: config)

        try writePNG(
            image,
            marks: request.marks ?? [],
            screenFrame: display.flatMap(screenFrame),
            to: outPath)
        return Result(
            width: image.width, height: image.height, path: outPath, mediaType: "image/png")
    }

    /// Resolve the requested target into an `SCContentFilter` and the pixel
    /// size to capture at.
    private static func buildFilter(
        _ target: CaptureTargetKind, request: HelperRequest, content: SCShareableContent
    ) throws -> (SCContentFilter, (width: Int, height: Int), SCDisplay?) {
        switch target {
        case .display:
            let display = display(for: request.displayId, in: content)
            guard let display else {
                throw HelperError(code: .notFound, message: "no display available")
            }
            let filter = SCContentFilter(display: display, excludingWindows: [])
            return (filter, (display.width, display.height), display)

        case .window:
            guard let windowId = request.windowId else {
                throw HelperError(code: .invalidRequest, message: "window capture requires window_id")
            }
            guard let window = content.windows.first(where: { $0.windowID == windowId }) else {
                throw HelperError(code: .notFound, message: "window \(windowId) not found")
            }
            let filter = SCContentFilter(desktopIndependentWindow: window)
            let frame = window.frame
            return (filter, (Int(frame.width), Int(frame.height)), nil)

        case .app:
            guard let bundleId = request.bundleId else {
                throw HelperError(code: .invalidRequest, message: "app capture requires bundle_id")
            }
            guard let app = content.applications.first(where: { $0.bundleIdentifier == bundleId })
            else {
                throw HelperError(code: .notFound, message: "app \(bundleId) is not running")
            }
            guard let display = display(for: request.displayId, in: content) else {
                throw HelperError(code: .notFound, message: "no display available")
            }
            let filter = SCContentFilter(
                display: display, including: [app], exceptingWindows: [])
            return (filter, (display.width, display.height), display)
        }
    }

    private static func display(for displayId: UInt32?, in content: SCShareableContent) -> SCDisplay?
    {
        if let displayId {
            return content.displays.first { $0.displayID == displayId }
        }
        return content.displays.first
    }

    private static func screenFrame(for display: SCDisplay) -> CGRect? {
        NSScreen.screens.first { screen in
            guard let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")]
                as? NSNumber
            else { return false }
            return number.uint32Value == display.displayID
        }?.frame
    }

    private static func writePNG(
        _ image: CGImage, marks: [CaptureMark], screenFrame: CGRect?, to path: String
    ) throws {
        let rep = NSBitmapImageRep(cgImage: image)
        if !marks.isEmpty {
            drawMarks(
                marks, on: rep, imageWidth: image.width, imageHeight: image.height,
                screenFrame: screenFrame)
        }
        guard let data = rep.representation(using: .png, properties: [:]) else {
            throw HelperError(code: .operationFailed, message: "could not encode PNG")
        }
        do {
            try data.write(to: URL(fileURLWithPath: path))
        } catch {
            throw HelperError(
                code: .operationFailed, message: "could not write capture to disk: \(error)")
        }
    }

    private static func drawMarks(
        _ marks: [CaptureMark], on rep: NSBitmapImageRep, imageWidth: Int, imageHeight: Int,
        screenFrame: CGRect?
    ) {
        guard let graphics = NSGraphicsContext(bitmapImageRep: rep) else { return }
        let previous = NSGraphicsContext.current
        NSGraphicsContext.current = graphics
        defer { NSGraphicsContext.current = previous }

        let context = graphics.cgContext
        context.saveGState()
        defer { context.restoreGState() }

        context.setAllowsAntialiasing(true)
        context.setShouldAntialias(true)
        // Draw in the same top-left-origin coordinate space used by AX frames
        // and image pixels.
        context.translateBy(x: 0, y: CGFloat(imageHeight))
        context.scaleBy(x: 1, y: -1)

        let frame = screenFrame ?? CGRect(x: 0, y: 0, width: imageWidth, height: imageHeight)
        let scaleX = Double(imageWidth) / max(Double(frame.width), 1.0)
        let scaleY = Double(imageHeight) / max(Double(frame.height), 1.0)

        for mark in marks {
            let rect = CGRect(
                x: (mark.frame.x - Double(frame.minX)) * scaleX,
                y: (mark.frame.y - Double(frame.minY)) * scaleY,
                width: mark.frame.width * scaleX,
                height: mark.frame.height * scaleY)
            guard rect.width > 0, rect.height > 0 else { continue }
            guard rect.intersects(CGRect(x: 0, y: 0, width: imageWidth, height: imageHeight)) else {
                continue
            }
            drawTargetOutline(rect, in: context)
            drawBadge(
                number: mark.mark,
                center: CGPoint(x: rect.midX, y: rect.midY),
                imageWidth: CGFloat(imageWidth),
                imageHeight: CGFloat(imageHeight),
                in: context)
        }
    }

    private static func drawTargetOutline(_ rect: CGRect, in context: CGContext) {
        context.setStrokeColor(CGColor(red: 0.05, green: 0.35, blue: 1.0, alpha: 0.9))
        context.setLineWidth(2)
        context.stroke(rect.insetBy(dx: -2, dy: -2))
        context.setStrokeColor(CGColor(red: 1.0, green: 1.0, blue: 1.0, alpha: 0.85))
        context.setLineWidth(1)
        context.stroke(rect.insetBy(dx: -4, dy: -4))
    }

    private static func drawBadge(
        number: Int, center: CGPoint, imageWidth: CGFloat, imageHeight: CGFloat, in context: CGContext
    ) {
        let digits = Array(String(max(number, 0)))
        let digitWidth: CGFloat = 7
        let digitHeight: CGFloat = 11
        let digitSpacing: CGFloat = 2
        let paddingX: CGFloat = 5
        let badgeHeight: CGFloat = 21
        let digitsWidth = CGFloat(digits.count) * digitWidth
            + CGFloat(max(digits.count - 1, 0)) * digitSpacing
        let badgeWidth = max(22, digitsWidth + paddingX * 2)
        let x = clamp(center.x - badgeWidth / 2, min: 3, max: imageWidth - badgeWidth - 3)
        let y = clamp(center.y - badgeHeight / 2, min: 3, max: imageHeight - badgeHeight - 3)
        let badgeRect = CGRect(x: x, y: y, width: badgeWidth, height: badgeHeight)

        let path = CGPath(
            roundedRect: badgeRect, cornerWidth: badgeHeight / 2, cornerHeight: badgeHeight / 2,
            transform: nil)
        context.setFillColor(CGColor(red: 0.05, green: 0.35, blue: 1.0, alpha: 0.96))
        context.addPath(path)
        context.fillPath()
        context.setStrokeColor(CGColor(red: 1.0, green: 1.0, blue: 1.0, alpha: 0.95))
        context.setLineWidth(2)
        context.addPath(path)
        context.strokePath()

        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        var digitX = x + (badgeWidth - digitsWidth) / 2
        let digitY = y + (badgeHeight - digitHeight) / 2
        for digit in digits {
            drawDigit(
                digit, at: CGPoint(x: digitX, y: digitY), width: digitWidth, height: digitHeight,
                in: context)
            digitX += digitWidth + digitSpacing
        }
    }

    private static func drawDigit(
        _ digit: Character, at origin: CGPoint, width: CGFloat, height: CGFloat, in context: CGContext
    ) {
        let segmentsByDigit: [Character: [Int]] = [
            "0": [0, 1, 2, 3, 4, 5],
            "1": [1, 2],
            "2": [0, 1, 6, 4, 3],
            "3": [0, 1, 6, 2, 3],
            "4": [5, 6, 1, 2],
            "5": [0, 5, 6, 2, 3],
            "6": [0, 5, 6, 4, 2, 3],
            "7": [0, 1, 2],
            "8": [0, 1, 2, 3, 4, 5, 6],
            "9": [0, 1, 2, 3, 5, 6],
        ]
        guard let active = segmentsByDigit[digit] else { return }
        let t = max(width / 4.5, 1.3)
        let midY = origin.y + height / 2 - t / 2
        let segmentRects = [
            CGRect(x: origin.x + t, y: origin.y, width: width - 2 * t, height: t),
            CGRect(x: origin.x + width - t, y: origin.y + t, width: t, height: height / 2 - t),
            CGRect(x: origin.x + width - t, y: midY + t, width: t, height: height / 2 - t),
            CGRect(x: origin.x + t, y: origin.y + height - t, width: width - 2 * t, height: t),
            CGRect(x: origin.x, y: midY + t, width: t, height: height / 2 - t),
            CGRect(x: origin.x, y: origin.y + t, width: t, height: height / 2 - t),
            CGRect(x: origin.x + t, y: midY, width: width - 2 * t, height: t),
        ]
        for index in active {
            context.fill(segmentRects[index])
        }
    }

    private static func clamp(_ value: CGFloat, min minValue: CGFloat, max maxValue: CGFloat)
        -> CGFloat
    {
        Swift.max(minValue, Swift.min(value, maxValue))
    }
}
