import AVFoundation
import CoreImage
import CoreVideo
import Foundation

/// Draws one export frame from a Layout. Pure with respect to its inputs: the same source frames
/// and time always give the same picture.
final class FrameRenderer {
    static let background = CIColor(red: 0.055, green: 0.059, blue: 0.075)

    let layout: Layout
    let focusX: Double?
    let drawsBorder: Bool
    private let context = CIContext(options: [.cacheIntermediates: false])
    private let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!

    init(layout: Layout, focusX: Double?, drawsBorder: Bool) {
        self.layout = layout
        self.focusX = focusX
        self.drawsBorder = drawsBorder
    }

    /// Layout rects are top-left; Core Image is bottom-left.
    private func flip(_ r: CGRect, height: CGFloat) -> CGRect {
        CGRect(x: r.minX, y: height - r.maxY, width: r.width, height: r.height)
    }

    func render(screen: CVPixelBuffer?, camera: CVPixelBuffer?, time: Double, into output: CVPixelBuffer) {
        let canvas = CGRect(origin: .zero, size: layout.canvas)
        var image = CIImage(color: Self.background).cropped(to: canvas)

        if let screen {
            image = composite(screen: CIImage(cvPixelBuffer: screen), over: image)
        }
        if let camera {
            image = composite(camera: CIImage(cvPixelBuffer: camera), over: image)
            if drawsBorder, let ring = ring(at: time) {
                image = ring.composited(over: image)
            }
        }
        context.render(image.cropped(to: canvas), to: output, bounds: canvas, colorSpace: colorSpace)
    }

    private func place(_ source: CIImage, crop: CGRect, into dest: CGRect) -> CIImage {
        let sourceHeight = source.extent.height
        let cropCI = flip(crop, height: sourceHeight)
        let destCI = flip(dest, height: layout.canvas.height)
        let scale = destCI.width / cropCI.width
        return source
            .cropped(to: cropCI)
            .transformed(by: CGAffineTransform(translationX: -cropCI.minX, y: -cropCI.minY))
            .transformed(by: CGAffineTransform(scaleX: scale, y: destCI.height / cropCI.height))
            .transformed(by: CGAffineTransform(translationX: destCI.minX, y: destCI.minY))
            .cropped(to: destCI)
    }

    private func composite(screen: CIImage, over background: CIImage) -> CIImage {
        let geometry = layout.screenGeometry(source: screen.extent.size, focusX: focusX)
        let placed = place(screen, crop: geometry.crop, into: geometry.dest)
        guard geometry.cornerRadius > 0 else { return placed.composited(over: background) }
        let mask = CIFilter(name: "CIRoundedRectangleGenerator", parameters: [
            "inputExtent": CIVector(cgRect: flip(geometry.dest, height: layout.canvas.height)),
            "inputRadius": geometry.cornerRadius,
            "inputColor": CIColor.white,
        ])!.outputImage!
        return placed.applyingFilter("CIBlendWithMask", parameters: [
            kCIInputBackgroundImageKey: background,
            kCIInputMaskImageKey: mask,
        ])
    }

    private func composite(camera: CIImage, over background: CIImage) -> CIImage {
        let slot = layout.camera.rect
        let placed = place(camera, crop: layout.cameraCrop(source: camera.extent.size), into: slot)
        let destCI = flip(slot, height: layout.canvas.height)
        let radius = destCI.width / 2
        let mask = CIFilter(name: "CIRadialGradient", parameters: [
            "inputCenter": CIVector(x: destCI.midX, y: destCI.midY),
            "inputRadius0": radius - 1.5,
            "inputRadius1": radius,
            "inputColor0": CIColor.white,
            "inputColor1": CIColor.clear,
        ])!.outputImage!.cropped(to: destCI)
        return placed.applyingFilter("CIBlendWithMask", parameters: [
            kCIInputBackgroundImageKey: background,
            kCIInputMaskImageKey: mask,
        ])
    }

    /// The attention ring: a rotating conic gradient annulus hugging the outside of the camera
    /// circle, with a blurred copy underneath for glow.
    private func ring(at time: Double) -> CIImage? {
        let b = border(at: time)
        guard b.opacity > 0.001 else { return nil }
        let slot = flip(layout.camera.rect, height: layout.canvas.height)
        let center = CGPoint(x: slot.midX, y: slot.midY)
        let inner = slot.width / 2
        let outer = inner + CGFloat(b.width) * slot.width

        let rotate = CGAffineTransform(translationX: -conic.extent.midX, y: -conic.extent.midY)
            .concatenating(CGAffineTransform(rotationAngle: CGFloat(b.rotation)))
            .concatenating(CGAffineTransform(translationX: center.x, y: center.y))
        let gradient = conic.transformed(by: rotate)

        let outsideInner = radial(center, from: inner - 0.75, to: inner + 0.75, inside: .clear, outside: .white)
        let insideOuter = radial(center, from: outer - 0.75, to: outer + 0.75, inside: .white, outside: .clear)
        let mask = insideOuter.applyingFilter("CIMultiplyCompositing", parameters: [kCIInputBackgroundImageKey: outsideInner])
        let bounds = CGRect(x: center.x - outer - 2, y: center.y - outer - 2, width: (outer + 2) * 2, height: (outer + 2) * 2)
        var ring = gradient
            .applyingFilter("CIBlendWithMask", parameters: [
                kCIInputBackgroundImageKey: CIImage.empty(),
                kCIInputMaskImageKey: mask,
            ])
            .cropped(to: bounds)
            .applyingFilter("CIColorMatrix", parameters: ["inputAVector": CIVector(x: 0, y: 0, z: 0, w: CGFloat(b.opacity))])

        let glowRadius = CGFloat(b.glow) * slot.width * 0.06
        if glowRadius > 0.5 {
            let glow = ring
                .applyingFilter("CIGaussianBlur", parameters: [kCIInputRadiusKey: glowRadius])
                .applyingFilter("CIColorMatrix", parameters: ["inputAVector": CIVector(x: 0, y: 0, z: 0, w: CGFloat(min(1, b.glow * 1.8)))])
            ring = ring.composited(over: glow)
        }
        return ring
    }

    private func radial(_ center: CGPoint, from r0: CGFloat, to r1: CGFloat, inside: CIColor, outside: CIColor) -> CIImage {
        CIFilter(name: "CIRadialGradient", parameters: [
            "inputCenter": CIVector(x: center.x, y: center.y),
            "inputRadius0": max(0, r0),
            "inputRadius1": r1,
            "inputColor0": inside,
            "inputColor1": outside,
        ])!.outputImage!
    }

    /// A square conic gradient of the ring palette, built once per export and rotated per frame.
    /// Core Graphics has no conic gradient in its Swift API and Core Image has no conic generator.
    private lazy var conic: CIImage = {
        let side = Int(ceil(layout.camera.rect.width * 1.5))
        var pixels = [UInt8](repeating: 0, count: side * side * 4)
        let stops = Border.palette
        let segments = Double(stops.count - 1)
        let c = Double(side) / 2
        for y in 0..<side {
            for x in 0..<side {
                let angle = atan2(Double(y) + 0.5 - c, Double(x) + 0.5 - c)
                let u = (angle + .pi) / (2 * .pi) * segments
                let i = min(Int(u), stops.count - 2)
                let f = u - Double(i)
                let a = stops[i], z = stops[i + 1]
                let o = (y * side + x) * 4
                pixels[o] = UInt8(((a.r + (z.r - a.r) * f) * 255).rounded())
                pixels[o + 1] = UInt8(((a.g + (z.g - a.g) * f) * 255).rounded())
                pixels[o + 2] = UInt8(((a.b + (z.b - a.b) * f) * 255).rounded())
                pixels[o + 3] = 255
            }
        }
        return CIImage(
            bitmapData: Data(pixels),
            bytesPerRow: side * 4,
            size: CGSize(width: side, height: side),
            format: .RGBA8,
            colorSpace: colorSpace
        )
    }()
}

final class LayoutInstruction: NSObject, AVVideoCompositionInstructionProtocol {
    let timeRange: CMTimeRange
    let enablePostProcessing = false
    let containsTweening = true
    let requiredSourceTrackIDs: [NSValue]?
    let passthroughTrackID = kCMPersistentTrackID_Invalid
    let screenTrackID: CMPersistentTrackID
    let cameraTrackID: CMPersistentTrackID?
    let renderer: FrameRenderer

    init(timeRange: CMTimeRange, screenTrackID: CMPersistentTrackID, cameraTrackID: CMPersistentTrackID?, renderer: FrameRenderer) {
        self.timeRange = timeRange
        self.screenTrackID = screenTrackID
        self.cameraTrackID = cameraTrackID
        self.renderer = renderer
        requiredSourceTrackIDs = ([screenTrackID] + (cameraTrackID.map { [$0] } ?? [])).map { NSNumber(value: $0) }
    }
}

final class LayoutCompositor: NSObject, AVVideoCompositing {
    private let queue = DispatchQueue(label: "rec.compositor")

    let sourcePixelBufferAttributes: [String: any Sendable]? = [
        kCVPixelBufferPixelFormatTypeKey as String: [kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange],
    ]
    let requiredPixelBufferAttributesForRenderContext: [String: any Sendable] = [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
    ]

    func renderContextChanged(_ newRenderContext: AVVideoCompositionRenderContext) {}

    func startRequest(_ request: AVAsynchronousVideoCompositionRequest) {
        queue.async {
            guard let instruction = request.videoCompositionInstruction as? LayoutInstruction,
                  let output = request.renderContext.newPixelBuffer() else {
                request.finish(with: CommandError(.exportFailed, "compositor could not get an output buffer"))
                return
            }
            let screen = request.sourceFrame(byTrackID: instruction.screenTrackID)
            let camera = instruction.cameraTrackID.flatMap { request.sourceFrame(byTrackID: $0) }
            instruction.renderer.render(screen: screen, camera: camera, time: request.compositionTime.seconds, into: output)
            request.finish(withComposedVideoFrame: output)
        }
    }

    func cancelAllPendingVideoCompositionRequests() {}
}
