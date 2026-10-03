import Foundation

/// The look of the attention ring at one instant. `width` is a fraction of the camera diameter
/// so the same curve drives the 300px export bubble and the on-screen preview.
public struct BorderAppearance: Equatable, Sendable {
    public var rotation: Double
    public var width: Double
    public var glow: Double
    public var opacity: Double
}

public enum Border {
    public static let introDuration = 1.5
    public static let calmWidth = 0.022
    public static let calmOpacity = 0.9
    /// Radians per second once calm.
    public static let calmSpin = 0.6

    private static let introWidth = 0.05
    private static let pulseWidth = 0.03
    private static let pulseCenter = 0.95
    private static let pulseHalfWidth = 0.25
    private static let introExtraTurns = 1.25

    public static func appearance(at t: Double) -> BorderAppearance {
        let t = max(t, 0)
        let fadeIn = easeOut(clamp01(t / 0.35))
        let settle = smoothstep(0.6, introDuration, t)
        let pulse = bump(t, center: pulseCenter, halfWidth: pulseHalfWidth)

        let rotation = calmSpin * t + introExtraTurns * 2 * .pi * easeOut(clamp01(t / introDuration))
        let width = calmWidth + (introWidth - calmWidth) * (1 - settle) + pulseWidth * pulse
        let glow = fadeIn * (1 - smoothstep(0.9, introDuration, t)) * (0.6 + 0.4 * pulse)
        let opacity = fadeIn * (1 + (calmOpacity - 1) * smoothstep(1.0, introDuration, t))
        return BorderAppearance(rotation: rotation, width: width, glow: glow, opacity: opacity)
    }

    private static func clamp01(_ x: Double) -> Double { min(max(x, 0), 1) }

    private static func easeOut(_ x: Double) -> Double { 1 - pow(1 - x, 3) }

    private static func smoothstep(_ a: Double, _ b: Double, _ x: Double) -> Double {
        let u = clamp01((x - a) / (b - a))
        return u * u * (3 - 2 * u)
    }

    private static func bump(_ x: Double, center: Double, halfWidth: Double) -> Double {
        let u = (x - center) / halfWidth
        guard abs(u) < 1 else { return 0 }
        return 0.5 * (1 + cos(.pi * u))
    }
}

public func border(at t: Double) -> BorderAppearance {
    Border.appearance(at: t)
}

extension Border {
    /// Conic gradient stops, shared by the preview ring and the export ring. The last equals the
    /// first so the sweep has no seam.
    public static let palette: [(r: Double, g: Double, b: Double)] = [
        (1.00, 0.24, 0.50),
        (1.00, 0.69, 0.24),
        (0.24, 0.85, 1.00),
        (0.54, 0.36, 1.00),
        (1.00, 0.24, 0.50),
    ]
}
