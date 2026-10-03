import AppKit
import Foundation

/// Samples the cursor at 30 Hz and records clicks, normalized to the source frame. Main thread only.
@MainActor
final class InputMonitor {
    private let frame: CGRect
    private let clock: () -> Double
    private var timer: Timer?
    private var clickMonitor: Any?
    private var lastCursor: NormalizedPoint?
    private(set) var events: [TimelineEvent] = []

    /// `frame` is the source frame in global top-left points; `clock` returns take-clock seconds.
    init(frame: CGRect, clock: @escaping () -> Double) {
        self.frame = frame
        self.clock = clock
    }

    func start() {
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.sampleCursor() }
        }
        clickMonitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]) { [weak self] event in
            MainActor.assumeIsolated { self?.recordClick(event) }
        }
        if clickMonitor == nil {
            Output.log("click monitoring unavailable; recording cursor only")
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        if let clickMonitor { NSEvent.removeMonitor(clickMonitor) }
        clickMonitor = nil
    }

    private func sampleCursor() {
        guard let p = normalizedCursor(), p != lastCursor else { return }
        lastCursor = p
        events.append(.cursor(t: clock(), at: p))
    }

    private func recordClick(_ event: NSEvent) {
        guard let p = normalizedCursor() else { return }
        let button: MouseButton = switch event.type {
        case .leftMouseDown: .left
        case .rightMouseDown: .right
        default: .other
        }
        events.append(.click(t: clock(), at: p, button: button))
    }

    /// NSEvent.mouseLocation is bottom-left origin on the primary screen; sources use top-left.
    private func normalizedCursor() -> NormalizedPoint? {
        let cocoa = NSEvent.mouseLocation
        let primaryHeight = NSScreen.screens.first?.frame.height ?? 0
        let x = (cocoa.x - frame.minX) / frame.width
        let y = (primaryHeight - cocoa.y - frame.minY) / frame.height
        guard (0...1).contains(x), (0...1).contains(y) else { return nil }
        return NormalizedPoint(x: (x * 1000).rounded() / 1000, y: (y * 1000).rounded() / 1000)
    }
}
