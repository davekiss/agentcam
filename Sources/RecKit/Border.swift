import Foundation

/// The look of the risograph attention ring at one instant. Lengths are fractions of the camera
/// diameter so the same curve drives the export bubble and the on-screen preview.
public struct BorderAppearance: Equatable, Sendable {
    /// Stroke width of each ink plate's ring.
    public var width: Double
    /// How far each plate sits from true registration.
    public var spread: Double
    public var opacity: Double
    /// Riso animation steps at a low frame rate; this indexes the step for jitter and grain.
    public var boilFrame: Int
}

/// One spot ink. `drift` is the direction its plate slides when out of register.
public struct Ink: Sendable {
    public var r: Double
    public var g: Double
    public var b: Double
    public var drift: Double
}

public enum Border {
    public static let introDuration = 1.5
    public static let calmWidth = 0.022
    public static let calmOpacity = 0.95
    public static let calmSpread = 0.007
    public static let boilFPS = 10.0
    /// Per-step plate wobble, independent of spread.
    public static let boilJitter = 0.004

    private static let introWidth = 0.05
    private static let pulseWidth = 0.03
    private static let pulseCenter = 0.95
    private static let pulseHalfWidth = 0.25
    private static let introSpread = 0.08
    private static let registerBy = 0.75
    private static let kickSpread = 0.025
    private static let kickHalfWidth = 0.2

    /// Fluorescent pink, riso blue, yellow: three plates drifting 120 degrees apart.
    public static let inks: [Ink] = [
        Ink(r: 1.00, g: 0.28, b: 0.69, drift: 200 * .pi / 180),
        Ink(r: 0.00, g: 0.47, b: 0.75, drift: 320 * .pi / 180),
        Ink(r: 1.00, g: 0.91, b: 0.00, drift: 80 * .pi / 180),
    ]

    public static func appearance(at t: Double) -> BorderAppearance {
        let t = max(t, 0)
        let fadeIn = easeOut(clamp01(t / 0.15))
        let settle = smoothstep(0.6, introDuration, t)
        let pulse = bump(t, center: pulseCenter, halfWidth: pulseHalfWidth)

        let width = calmWidth + (introWidth - calmWidth) * (1 - settle) + pulseWidth * pulse
        let spread = calmSpread
            + (introSpread - calmSpread) * (1 - easeOut(clamp01(t / registerBy)))
            + kickSpread * bump(t, center: pulseCenter, halfWidth: kickHalfWidth)
        let opacity = fadeIn * (1 + (calmOpacity - 1) * smoothstep(1.0, introDuration, t))
        return BorderAppearance(width: width, spread: spread, opacity: opacity, boilFrame: Int(floor(t * boilFPS)))
    }

    /// Where an ink plate's ring is centered relative to the camera circle, +x right and +y up.
    public static func plateOffset(_ index: Int, _ b: BorderAppearance) -> (dx: Double, dy: Double) {
        let drift = inks[index].drift
        return (
            dx: b.spread * cos(drift) + boilJitter * noise(b.boilFrame, salt: index * 2),
            dy: b.spread * sin(drift) + boilJitter * noise(b.boilFrame, salt: index * 2 + 1)
        )
    }

    /// Deterministic hash noise in -1...1, so a re-export draws the same wobble.
    static func noise(_ frame: Int, salt: Int) -> Double {
        var h = UInt64(truncatingIfNeeded: frame &* 73_856_093 ^ salt &* 19_349_663)
        h ^= h >> 33
        h &*= 0xff51_afd7_ed55_8ccd
        h ^= h >> 33
        h &*= 0xc4ce_b9fe_1a85_ec53
        h ^= h >> 33
        return Double(h % 20_001) / 10_000 - 1
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
