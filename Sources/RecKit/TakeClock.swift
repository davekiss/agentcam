import CoreMedia
import Foundation

/// The take clock: seconds since t0, a host-clock time. Capture sample timestamps are host-clock
/// times, so a track's offset is its first sample minus t0.
public struct TakeClock: Equatable, Sendable {
    public let t0: Double

    public init(t0: Double) {
        self.t0 = t0
    }

    public static func hostNow() -> Double {
        CMClockGetTime(CMClockGetHostTimeClock()).seconds
    }

    public func time(ofHost host: Double) -> Double {
        host - t0
    }

    /// A track's offset is clamped at zero: a sample stamped before t0 belongs at the start.
    public func offset(firstSampleHost host: Double) -> Double {
        max(0, ((host - t0) * 1000).rounded() / 1000)
    }
}
