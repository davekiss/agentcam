import AppKit
import CoreGraphics
import Foundation
@preconcurrency import ScreenCaptureKit

public enum SourceSelector: Equatable, Sendable {
    case mainDisplay
    case display(UInt32)
    case window(UInt32)
    case app(String)
}

public enum ResolvedSource {
    case display(SCDisplay, Source)
    case window(SCWindow, Source)

    public var source: Source {
        switch self {
        case let .display(_, s), let .window(_, s): return s
        }
    }
}

public struct SourceList: Encodable {
    public var displays: [Source]
    public var windows: [Source]
}

public enum SourceCatalog {
    public static func ensureScreenRecordingPermission() throws {
        guard CGPreflightScreenCaptureAccess() else {
            // Registers the request so the app shows up in System Settings; returns immediately.
            CGRequestScreenCaptureAccess()
            throw screenPermissionError
        }
    }

    static let screenPermissionError = CommandError(
        .permissionDenied,
        "Screen Recording permission is not granted. Allow it for your terminal app in System Settings > Privacy & Security > Screen & System Audio Recording, then restart the terminal."
    )

    public static func shareableContent() async throws -> SCShareableContent {
        try ensureScreenRecordingPermission()
        do {
            return try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
        } catch {
            let ns = error as NSError
            if ns.domain == SCStreamErrorDomain, ns.code == SCStreamError.userDeclined.rawValue {
                throw screenPermissionError
            }
            throw CommandError(.recorderFailed, "could not read shareable content: \(error.localizedDescription)")
        }
    }

    public static func list() async throws -> SourceList {
        let content = try await shareableContent()
        let displays = await MainActor.run { content.displays.map(describe) }
        let windows = content.windows.filter(isUserWindow).map(describe)
        return SourceList(displays: displays, windows: windows)
    }

    public static func resolve(_ selector: SourceSelector, in content: SCShareableContent) async throws -> ResolvedSource {
        switch selector {
        case .mainDisplay:
            let mainID = CGMainDisplayID()
            guard let d = content.displays.first(where: { $0.displayID == mainID }) ?? content.displays.first else {
                throw CommandError(.sourceNotFound, "no display available")
            }
            return .display(d, await MainActor.run { describe(d) })
        case let .display(id):
            guard let d = content.displays.first(where: { $0.displayID == id }) else {
                throw CommandError(.sourceNotFound, "no display with id \(id); run `agentcam-mac sources`")
            }
            return .display(d, await MainActor.run { describe(d) })
        case let .window(id):
            guard let w = content.windows.first(where: { $0.windowID == id }) else {
                throw CommandError(.sourceNotFound, "no on-screen window with id \(id); run `agentcam-mac sources`")
            }
            return .window(w, describe(w))
        case let .app(name):
            // SCShareableContent lists windows front to back, so the first match is the frontmost.
            let needle = name.lowercased()
            guard let w = content.windows.filter(isUserWindow).first(where: {
                $0.owningApplication?.applicationName.lowercased() == needle
                    || $0.owningApplication?.bundleIdentifier.lowercased() == needle
            }) else {
                throw CommandError(.sourceNotFound, "no on-screen window for app \"\(name)\"; run `agentcam-mac sources`")
            }
            return .window(w, describe(w))
        }
    }

    static func isUserWindow(_ w: SCWindow) -> Bool {
        w.windowLayer == 0 && w.frame.width >= 64 && w.frame.height >= 64 && w.owningApplication != nil
            && w.owningApplication?.processID != ProcessInfo.processInfo.processIdentifier
    }

    @MainActor
    static func describe(_ d: SCDisplay) -> Source {
        let screen = NSScreen.screens.first {
            ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == d.displayID
        }
        return Source(
            kind: .display,
            id: d.displayID,
            title: screen?.localizedName ?? "Display \(d.displayID)",
            app: nil,
            frame: Rect(d.frame),
            scale: Double(screen?.backingScaleFactor ?? 1)
        )
    }

    static func describe(_ w: SCWindow) -> Source {
        Source(
            kind: .window,
            id: w.windowID,
            title: w.title ?? "",
            app: w.owningApplication?.applicationName,
            frame: Rect(w.frame),
            scale: Double(scaleForWindow(w.frame))
        )
    }

    private static func scaleForWindow(_ frame: CGRect) -> CGFloat {
        var count: UInt32 = 0
        var ids = [CGDirectDisplayID](repeating: 0, count: 8)
        CGGetDisplaysWithRect(frame, 8, &ids, &count)
        guard count > 0, let mode = CGDisplayCopyDisplayMode(ids[0]) else { return 1 }
        return CGFloat(mode.pixelWidth) / CGFloat(mode.width)
    }
}

extension Rect {
    init(_ r: CGRect) {
        self.init(x: Double(r.origin.x), y: Double(r.origin.y), width: Double(r.width), height: Double(r.height))
    }
}
