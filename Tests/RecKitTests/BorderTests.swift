import XCTest
@testable import RecKit

final class BorderTests: XCTestCase {
    func testStartsInvisible() {
        XCTAssertEqual(border(at: 0).opacity, 0)
        XCTAssertEqual(border(at: -1), border(at: 0))
    }

    func testPlatesStartFarOutOfRegisterThenSnapIn() {
        XCTAssertGreaterThan(border(at: 0).spread, Border.calmSpread * 10)
        XCTAssertEqual(border(at: 0.75).spread, Border.calmSpread, accuracy: 1e-3, "registered by 0.75s")
        XCTAssertGreaterThan(border(at: 0.95).spread, border(at: 0.75).spread * 3, "kick on the beat")
        XCTAssertEqual(border(at: 0.5).opacity, 1, accuracy: 1e-9)
    }

    func testIntroRingIsThickAndPulses() {
        let peak = border(at: 0.95)
        XCTAssertGreaterThan(peak.width, Border.calmWidth * 2.5)
        XCTAssertGreaterThan(peak.width, border(at: 0.5).width)
        XCTAssertGreaterThan(peak.width, border(at: 1.3).width)
    }

    func testCalmAfterIntroIsConstantExceptBoil() {
        for t in stride(from: Border.introDuration, through: 120, by: 0.37) {
            let b = border(at: t)
            XCTAssertEqual(b.width, Border.calmWidth, accuracy: 1e-12, "t=\(t)")
            XCTAssertEqual(b.spread, Border.calmSpread, accuracy: 1e-12, "t=\(t)")
            XCTAssertEqual(b.opacity, Border.calmOpacity, accuracy: 1e-12, "t=\(t)")
        }
    }

    func testBoilStepsAtLowFrameRate() {
        XCTAssertEqual(border(at: 2.01).boilFrame, border(at: 2.09).boilFrame)
        XCTAssertEqual(border(at: 2.11).boilFrame, border(at: 2.01).boilFrame + 1)
        let a = Border.plateOffset(0, border(at: 2.01))
        let b = Border.plateOffset(0, border(at: 2.11))
        XCTAssertNotEqual(a.dx, b.dx, "plates wobble between steps")
    }

    func testCalmPlatesStayNearlyRegistered() {
        let limit = Border.calmSpread + Border.boilJitter * 2.squareRoot() + 1e-12
        for t in stride(from: 2.0, through: 30, by: 0.1) {
            for index in Border.inks.indices {
                let o = Border.plateOffset(index, border(at: t))
                XCTAssertLessThanOrEqual(hypot(o.dx, o.dy), limit, "t=\(t) plate=\(index)")
            }
        }
    }

    func testPlatesDriftApartInDifferentDirections() {
        let start = border(at: 0)
        let offsets = Border.inks.indices.map { Border.plateOffset($0, start) }
        for i in offsets.indices {
            for j in offsets.indices where j > i {
                XCTAssertGreaterThan(hypot(offsets[i].dx - offsets[j].dx, offsets[i].dy - offsets[j].dy), start.spread)
            }
        }
    }

    func testNoiseIsDeterministicAndBounded() {
        for frame in 0..<500 {
            let n = Border.noise(frame, salt: 3)
            XCTAssertEqual(n, Border.noise(frame, salt: 3))
            XCTAssertLessThanOrEqual(abs(n), 1)
        }
    }

    func testAppearanceIsContinuousAcrossIntroBoundary() {
        let before = border(at: Border.introDuration - 1e-4)
        let after = border(at: Border.introDuration + 1e-4)
        XCTAssertEqual(before.width, after.width, accuracy: 1e-3)
        XCTAssertEqual(before.spread, after.spread, accuracy: 1e-3)
        XCTAssertEqual(before.opacity, after.opacity, accuracy: 1e-3)
    }
}
