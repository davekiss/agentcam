import XCTest
@testable import RecKit

final class BorderTests: XCTestCase {
    func testStartsInvisible() {
        XCTAssertEqual(border(at: 0).opacity, 0)
        XCTAssertEqual(border(at: -1), border(at: 0))
    }

    func testIntroIsThickGlowingAndPulses() {
        let peak = border(at: 0.95)
        XCTAssertGreaterThan(peak.width, Border.calmWidth * 2.5)
        XCTAssertGreaterThan(peak.glow, 0.5)
        XCTAssertGreaterThan(peak.width, border(at: 0.5).width, "the pulse widens the ring")
        XCTAssertGreaterThan(peak.width, border(at: 1.3).width)
        XCTAssertEqual(border(at: 0.5).opacity, 1, accuracy: 1e-9)
    }

    func testCalmAfterIntroIsConstantExceptRotation() {
        for t in stride(from: Border.introDuration, through: 120, by: 0.37) {
            let b = border(at: t)
            XCTAssertEqual(b.width, Border.calmWidth, accuracy: 1e-12, "t=\(t)")
            XCTAssertEqual(b.glow, 0, accuracy: 1e-12, "t=\(t)")
            XCTAssertEqual(b.opacity, Border.calmOpacity, accuracy: 1e-12, "t=\(t)")
        }
    }

    func testRotationIsContinuousAndKeepsSpinning() {
        var previous = border(at: 0).rotation
        for step in 1...600 {
            let r = border(at: Double(step) / 60).rotation
            XCTAssertGreaterThan(r, previous)
            XCTAssertLessThan(r - previous, 0.5, "no jumps at step \(step)")
            previous = r
        }
        XCTAssertEqual(border(at: 11).rotation - border(at: 10).rotation, Border.calmSpin, accuracy: 1e-9)
    }

    func testAppearanceIsContinuousAcrossIntroBoundary() {
        let before = border(at: Border.introDuration - 1e-4)
        let after = border(at: Border.introDuration + 1e-4)
        XCTAssertEqual(before.width, after.width, accuracy: 1e-3)
        XCTAssertEqual(before.glow, after.glow, accuracy: 1e-3)
        XCTAssertEqual(before.opacity, after.opacity, accuracy: 1e-3)
    }
}
