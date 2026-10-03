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
    private let gradient = CAGradientLayer()
    private let ringMask = CAShapeLayer()
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
        ringContainer.shadowColor = NSColor.white.cgColor
        ringContainer.shadowOffset = .zero
        ringContainer.opacity = 0
        gradient.type = .conic
        gradient.frame = root.bounds
        gradient.startPoint = CGPoint(x: 0.5, y: 0.5)
        gradient.endPoint = CGPoint(x: 0.5, y: 0)
        gradient.colors = Border.palette.map { CGColor(red: $0.r, green: $0.g, blue: $0.b, alpha: 1) }
        ringMask.frame = root.bounds
        ringMask.fillColor = nil
        ringMask.strokeColor = NSColor.black.cgColor
        gradient.mask = ringMask
        ringContainer.addSublayer(gradient)
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
        let center = CGPoint(x: ringMask.bounds.midX, y: ringMask.bounds.midY)
        ringMask.path = CGPath(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2), transform: nil)
        ringMask.lineWidth = width
        gradient.setAffineTransform(CGAffineTransform(rotationAngle: CGFloat(b.rotation)))
        ringContainer.opacity = Float(b.opacity * fade)
        ringContainer.shadowOpacity = Float(b.glow)
        ringContainer.shadowRadius = CGFloat(b.glow) * 16
        CATransaction.commit()
    }
}
