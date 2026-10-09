@preconcurrency import AVFoundation
import CoreImage
import CoreVideo
import Foundation

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
            if drawsBorder {
                image = ring(at: time, over: image)
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

    /// The risograph attention ring: one grainy annulus per spot ink, each on its own
    /// misregistered plate, multiplied together the way overprinted inks mix.
    private func ring(at time: Double, over image: CIImage) -> CIImage {
        let b = border(at: time)
        guard b.opacity > 0.001 else { return image }
        let slot = flip(layout.camera.rect, height: layout.canvas.height)
        let diameter = slot.width
        let inner = diameter / 2
        let outer = inner + CGFloat(b.width) * diameter
        let reach = outer + CGFloat(b.spread + Border.boilJitter) * diameter + 2
        let bounds = CGRect(x: slot.midX - reach, y: slot.midY - reach, width: reach * 2, height: reach * 2)

        var inks = CIImage(color: .white).cropped(to: bounds)
        var coverage = CIImage(color: .black).cropped(to: bounds)
        for (index, ink) in Border.inks.enumerated() {
            let offset = Border.plateOffset(index, b)
            let center = CGPoint(x: slot.midX + CGFloat(offset.dx) * diameter, y: slot.midY + CGFloat(offset.dy) * diameter)
            let annulus = radial(center, from: outer - 0.75, to: outer + 0.75, inside: .white, outside: .black)
                .applyingFilter("CIMultiplyCompositing", parameters: [
                    kCIInputBackgroundImageKey: radial(center, from: inner - 0.75, to: inner + 0.75, inside: .black, outside: .white),
                ])
            let mask = annulus
                .applyingFilter("CIMultiplyCompositing", parameters: [kCIInputBackgroundImageKey: grain(plate: index, frame: b.boilFrame)])
                .cropped(to: bounds)
            let plate = CIImage(color: CIColor(red: ink.r, green: ink.g, blue: ink.b)).applyingFilter("CIBlendWithMask", parameters: [
                kCIInputBackgroundImageKey: CIImage(color: .white),
                kCIInputMaskImageKey: mask,
            ])
            inks = plate.applyingFilter("CIMultiplyCompositing", parameters: [kCIInputBackgroundImageKey: inks]).cropped(to: bounds)
            coverage = mask.applyingFilter("CIMaximumCompositing", parameters: [kCIInputBackgroundImageKey: coverage]).cropped(to: bounds)
        }
        let fade = CGFloat(b.opacity)
        let alpha = coverage.applyingFilter("CIColorMatrix", parameters: [
            "inputRVector": CIVector(x: fade, y: 0, z: 0, w: 0),
            "inputGVector": CIVector(x: 0, y: fade, z: 0, w: 0),
            "inputBVector": CIVector(x: 0, y: 0, z: fade, w: 0),
        ])
        return inks.applyingFilter("CIBlendWithMask", parameters: [
            kCIInputBackgroundImageKey: image,
            kCIInputMaskImageKey: alpha,
        ])
    }

    /// Speckled ink coverage: mostly solid with pinhole voids, reshuffled on every boil step.
    private func grain(plate: Int, frame: Int) -> CIImage {
        let gain: CGFloat = 5
        let bias: CGFloat = -0.4 * gain + 0.4
        return CIFilter(name: "CIRandomGenerator")!.outputImage!
            .transformed(by: CGAffineTransform(translationX: CGFloat(frame * 97 + plate * 211), y: CGFloat(frame * 31 + plate * 157)))
            .transformed(by: CGAffineTransform(scaleX: 1.6, y: 1.6))
            .applyingFilter("CIColorMatrix", parameters: [
                "inputRVector": CIVector(x: gain, y: 0, z: 0, w: 0),
                "inputGVector": CIVector(x: gain, y: 0, z: 0, w: 0),
                "inputBVector": CIVector(x: gain, y: 0, z: 0, w: 0),
                "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 0),
                "inputBiasVector": CIVector(x: bias, y: bias, z: bias, w: 1),
            ])
            .applyingFilter("CIColorClamp")
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
    private let queue = DispatchQueue(label: "agentcam.compositor")

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
