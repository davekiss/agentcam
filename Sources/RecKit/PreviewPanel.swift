import AppKit
@preconcurrency import AVFoundation
import QuartzCore

/// The floating circular webcam bubble the presenter sees while recording. It is excluded from
/// the capture; the bubble in the final video is drawn at export.
@MainActor
final class PreviewPanel {
    private let diameter: CGFloat = 180
    private let margin: CGFloat = 40
    private let panel: NSPanel
    private let ringContainer = CALayer()
    private let plates: [CAShapeLayer] = Border.inks.map { _ in CAShapeLayer() }
    private let grain = CALayer()
    private var animation: Timer?

    var windowID: CGWindowID { CGWindowID(panel.windowNumber) }

    init(session: AVCaptureSession) {
        let side = diameter + margin * 2
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let origin = NSPoint(x: screen.maxX - side - 8, y: screen.minY + 8)
        panel = NSPanel(
            contentRect: NSRect(origin: origin, size: NSSize(width: side, height: side)),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = .floating
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.isMovableByWindowBackground = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]

        let content = NSView(frame: NSRect(x: 0, y: 0, width: side, height: side))
        content.wantsLayer = true
        panel.contentView = content
        let root = content.layer!

        let circle = CGRect(x: margin, y: margin, width: diameter, height: diameter)
        let video = AVCaptureVideoPreviewLayer(session: session)
        video.frame = circle
        video.videoGravity = .resizeAspectFill
        video.cornerRadius = diameter / 2
        video.masksToBounds = true
        video.backgroundColor = NSColor.black.cgColor
        if let connection = video.connection, connection.isVideoMirroringSupported {
            connection.automaticallyAdjustsVideoMirroring = false
            connection.isVideoMirrored = true
        }
        root.addSublayer(video)

        ringContainer.frame = root.bounds
        ringContainer.opacity = 0
        for (plate, ink) in zip(plates, Border.inks) {
            plate.frame = root.bounds
            plate.fillColor = nil
            plate.strokeColor = CGColor(red: ink.r, green: ink.g, blue: ink.b, alpha: 1)
            plate.compositingFilter = "multiplyBlendMode"
            ringContainer.addSublayer(plate)
        }
        // Oversized so it can shift per boil step and still cover the ring.
        grain.frame = root.bounds.insetBy(dx: -24, dy: -24)
        grain.contents = Self.grainImage(side: Int(grain.frame.width))
        grain.magnificationFilter = .nearest
        ringContainer.mask = grain
        root.addSublayer(ringContainer)
    }

    func show() {
        panel.orderFrontRegardless()
    }

    func close() {
        animation?.invalidate()
        panel.orderOut(nil)
    }

    /// Plays the border intro once, using the same curve the export uses, then fades the ring away.
    func playIntro() {
        let start = CACurrentMediaTime()
        let end = Border.introDuration + 0.4
        animation?.invalidate()
        animation = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self else { return timer.invalidate() }
                let t = CACurrentMediaTime() - start
                let fade = t <= Border.introDuration ? 1 : max(0, 1 - (t - Border.introDuration) / 0.4)
                self.apply(border(at: t), fade: fade)
                if t >= end { timer.invalidate() }
            }
        }
    }

    private func apply(_ b: BorderAppearance, fade: Double) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let width = CGFloat(b.width) * diameter
        let radius = diameter / 2 + width / 2
        for (index, plate) in plates.enumerated() {
            let offset = Border.plateOffset(index, b)
            let center = CGPoint(x: plate.bounds.midX + CGFloat(offset.dx) * diameter, y: plate.bounds.midY + CGFloat(offset.dy) * diameter)
            plate.path = CGPath(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2), transform: nil)
            plate.lineWidth = width
        }
        let jitter = CGFloat(Border.noise(b.boilFrame, salt: 99) * 20)
        grain.frame.origin = CGPoint(x: -24 + jitter, y: -24 - jitter)
        ringContainer.opacity = Float(b.opacity * fade)
        CATransaction.commit()
    }

    /// Alpha speckle matching the export's ink grain: mostly solid with pinhole voids.
    private static func grainImage(side: Int) -> CGImage? {
        var alpha = [UInt8](repeating: 0, count: side * side)
        var seed: UInt64 = 0x9e37_79b9_7f4a_7c15
        for i in alpha.indices {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            alpha[i] = (seed >> 56) < 40 ? 0 : 255
        }
        guard let provider = CGDataProvider(data: Data(alpha) as CFData) else { return nil }
        return CGImage(
            width: side, height: side, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: side,
            space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.alphaOnly.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
        )
    }
}
