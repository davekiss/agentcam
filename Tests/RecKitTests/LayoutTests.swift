import CoreGraphics
import XCTest
@testable import RecKit

final class LayoutTests: XCTestCase {
    private let retina = CGSize(width: 2880, height: 1800)

    func testEveryPresetKeepsRectsInsideCanvas() {
        for aspect in Aspect.allCases {
            let layout = Layout.preset(aspect)
            let canvas = CGRect(origin: .zero, size: layout.canvas)
            XCTAssertTrue(canvas.contains(layout.screen.region), "\(aspect) screen region")
            XCTAssertTrue(canvas.contains(layout.camera.rect), "\(aspect) camera")
            XCTAssertEqual(layout.camera.rect.width, layout.camera.rect.height, "\(aspect) circle is square")
            for source in [retina, CGSize(width: 1280, height: 2000), CGSize(width: 800, height: 600)] {
                let g = layout.screenGeometry(source: source, focusX: 0.9)
                XCTAssertTrue(canvas.insetBy(dx: -0.5, dy: -0.5).contains(g.dest), "\(aspect) dest for \(source)")
                XCTAssertTrue(CGRect(origin: .zero, size: source).insetBy(dx: -0.5, dy: -0.5).contains(g.crop), "\(aspect) crop for \(source)")
            }
        }
    }

    func testCanvasSizes() {
        XCTAssertEqual(Layout.preset(.landscape).canvas, CGSize(width: 1920, height: 1080))
        XCTAssertEqual(Layout.preset(.portrait).canvas, CGSize(width: 1080, height: 1920))
    }

    func testLandscapeFitsWholeScreenCenteredWithMargin() {
        let layout = Layout.preset(.landscape)
        let g = layout.screenGeometry(source: retina, focusX: 0.1)
        XCTAssertEqual(g.crop, CGRect(origin: .zero, size: retina), "fit never crops")
        XCTAssertEqual(g.dest.width / g.dest.height, retina.width / retina.height, accuracy: 1e-6)
        XCTAssertEqual(g.dest.midX, 960, accuracy: 1e-6)
        XCTAssertEqual(g.dest.midY, 540, accuracy: 1e-6)
        XCTAssertGreaterThan(g.dest.minY, 0)
        XCTAssertLessThan(g.dest.maxY, 1080)
        XCTAssertGreaterThan(layout.camera.rect.midX, 1440, "camera sits in the right quarter")
        XCTAssertGreaterThan(layout.camera.rect.midY, 720, "camera sits in the bottom third")
    }

    func testPortraitFillsTopRegionAndCentersOnFocus() {
        let layout = Layout.preset(.portrait)
        let region = layout.screen.region
        XCTAssertEqual(region.width, 1080)
        XCTAssertEqual(region.minY, 0)

        let centered = layout.screenGeometry(source: retina, focusX: 0.5)
        XCTAssertEqual(centered.dest, region)
        XCTAssertEqual(centered.crop.width / centered.crop.height, region.width / region.height, accuracy: 1e-6)
        XCTAssertEqual(centered.crop.midX, 1440, accuracy: 1e-6)

        let focused = layout.screenGeometry(source: retina, focusX: 0.4)
        XCTAssertEqual(focused.crop.midX, 0.4 * 2880, accuracy: 1e-6)

        let pinnedRight = layout.screenGeometry(source: retina, focusX: 0.99)
        XCTAssertEqual(pinnedRight.crop.maxX, 2880, accuracy: 1e-6, "crop clamps to the source edge")
        let pinnedLeft = layout.screenGeometry(source: retina, focusX: 0.0)
        XCTAssertEqual(pinnedLeft.crop.minX, 0, accuracy: 1e-6)

        XCTAssertGreaterThanOrEqual(layout.camera.rect.minY, region.maxY, "camera is below the screen")
        XCTAssertEqual(layout.camera.rect.midX, 540, accuracy: 1e-6)
    }

    func testCameraCropIsCenteredSquare() {
        let layout = Layout.preset(.landscape)
        XCTAssertEqual(layout.cameraCrop(source: CGSize(width: 1280, height: 720)), CGRect(x: 280, y: 0, width: 720, height: 720))
        XCTAssertEqual(layout.cameraCrop(source: CGSize(width: 720, height: 1280)), CGRect(x: 0, y: 280, width: 720, height: 720))
    }
}
