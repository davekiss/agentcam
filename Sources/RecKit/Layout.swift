import CoreGraphics
import Foundation

public enum Aspect: String, CaseIterable, Codable, Sendable {
    case landscape = "16:9"
    case portrait = "9:16"

    public var fileSuffix: String {
        rawValue.replacingOccurrences(of: ":", with: "x")
    }
}

/// How the screen recording occupies its region of the canvas.
public enum ScreenPlacement: Equatable, Sendable {
    /// Scale the whole screen to fit inside the region, letterboxed on the background.
    case fit(CGRect, cornerRadius: CGFloat)
    /// Scale the screen to cover the region and crop the overflow, centered on a focus point.
    case fill(CGRect)

    public var region: CGRect {
        switch self {
        case let .fit(r, _), let .fill(r): return r
        }
    }
}

public enum CameraShape: String, Sendable {
    case circle
}

public struct CameraSlot: Equatable, Sendable {
    public var rect: CGRect
    public var shape: CameraShape
}

/// Where the screen lands for one source size: the crop of the source (pixels, top-left origin)
/// and the rect it is drawn into on the canvas (pixels, top-left origin).
public struct ScreenGeometry: Equatable, Sendable {
    public var crop: CGRect
    public var dest: CGRect
    public var cornerRadius: CGFloat
}

/// All rects are in canvas pixels with a top-left origin. The compositor converts to Core Image's
/// bottom-left space in one place.
public struct Layout: Equatable, Sendable {
    public var aspect: Aspect
    public var canvas: CGSize
    public var screen: ScreenPlacement
    public var camera: CameraSlot

    public static let presets: [Aspect: Layout] = [
        .landscape: Layout(
            aspect: .landscape,
            canvas: CGSize(width: 1920, height: 1080),
            screen: .fit(CGRect(x: 48, y: 48, width: 1824, height: 984), cornerRadius: 14),
            camera: CameraSlot(rect: CGRect(x: 1920 - 56 - 300, y: 1080 - 56 - 300, width: 300, height: 300), shape: .circle)
        ),
        .portrait: Layout(
            aspect: .portrait,
            canvas: CGSize(width: 1080, height: 1920),
            screen: .fill(CGRect(x: 0, y: 0, width: 1080, height: 1200)),
            camera: CameraSlot(rect: CGRect(x: (1080 - 600) / 2, y: 1200 + (720 - 600) / 2, width: 600, height: 600), shape: .circle)
        ),
    ]

    public static func preset(_ aspect: Aspect) -> Layout {
        presets[aspect]!
    }

    /// `focusX` is the normalized (0..1) horizontal point to keep centered when cropping.
    public func screenGeometry(source: CGSize, focusX: Double?) -> ScreenGeometry {
        switch screen {
        case let .fit(region, radius):
            let scale = min(region.width / source.width, region.height / source.height)
            let size = CGSize(width: source.width * scale, height: source.height * scale)
            let dest = CGRect(
                x: region.midX - size.width / 2,
                y: region.midY - size.height / 2,
                width: size.width,
                height: size.height
            )
            return ScreenGeometry(crop: CGRect(origin: .zero, size: source), dest: dest, cornerRadius: radius)

        case let .fill(region):
            let scale = max(region.width / source.width, region.height / source.height)
            let cropSize = CGSize(width: region.width / scale, height: region.height / scale)
            let centerX = CGFloat(focusX ?? 0.5) * source.width
            let x = min(max(centerX - cropSize.width / 2, 0), source.width - cropSize.width)
            let y = (source.height - cropSize.height) / 2
            return ScreenGeometry(
                crop: CGRect(x: x, y: y, width: cropSize.width, height: cropSize.height),
                dest: region,
                cornerRadius: 0
            )
        }
    }

    /// Centered aspect-fill crop of the camera frame to the slot's aspect.
    public func cameraCrop(source: CGSize) -> CGRect {
        let slotAspect = camera.rect.width / camera.rect.height
        if source.width / source.height > slotAspect {
            let w = source.height * slotAspect
            return CGRect(x: (source.width - w) / 2, y: 0, width: w, height: source.height)
        }
        let h = source.width / slotAspect
        return CGRect(x: 0, y: (source.height - h) / 2, width: source.width, height: h)
    }
}
