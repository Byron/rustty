import AppKit
import SwiftUI
import Testing
@testable import Ghostty

@Suite
struct SurfaceHighlightTests {
    @Test func focusFrameIsHalfTheZoomFrameWidth() {
        #expect(Ghostty.OSSurfaceView.FiniteHighlight.focus.lineWidth == 1.5)
        #expect(Ghostty.OSSurfaceView.FiniteHighlight.zoom.lineWidth == 3)
    }

    @MainActor @Test func focusFrameUsesProvidedAccentColor() throws {
        let expected = NSColor(srgbRed: 0.12, green: 0.73, blue: 0.41, alpha: 1)
        let view = NSHostingView(rootView:
            Ghostty.FiniteHighlightOverlay(highlight: .zoom)
                .ghosttyAccentColor(Color(nsColor: expected)))
        view.frame = NSRect(x: 0, y: 0, width: 20, height: 20)
        view.layoutSubtreeIfNeeded()

        let image = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: image)
        let actual = try #require(image.colorAt(x: 1, y: 10)?.usingColorSpace(.sRGB))

        #expect(abs(actual.redComponent - expected.redComponent) < 0.005)
        #expect(abs(actual.greenComponent - expected.greenComponent) < 0.005)
        #expect(abs(actual.blueComponent - expected.blueComponent) < 0.005)
    }

    @Test func navigationWarningFlashesTwice() {
        let start = Date(timeIntervalSinceReferenceDate: 100)
        let warning = Ghostty.OSSurfaceView.NavigationWarning(
            scope: .panel,
            startedAt: start)

        #expect(warning.isVisible(at: start.addingTimeInterval(0.149), reduceMotion: false))
        #expect(!warning.isVisible(at: start.addingTimeInterval(0.15), reduceMotion: false))
        #expect(!warning.isVisible(at: start.addingTimeInterval(0.249), reduceMotion: false))
        #expect(warning.isVisible(at: start.addingTimeInterval(0.25), reduceMotion: false))
        #expect(!warning.isVisible(at: start.addingTimeInterval(0.4), reduceMotion: false))
    }

    @Test func navigationWarningIsSteadyWithReducedMotion() {
        let start = Date(timeIntervalSinceReferenceDate: 100)
        let warning = Ghostty.OSSurfaceView.NavigationWarning(
            scope: .quadrant,
            startedAt: start)

        #expect(warning.isVisible(at: start.addingTimeInterval(0.2), reduceMotion: true))
        #expect(!warning.isVisible(at: start.addingTimeInterval(0.4), reduceMotion: true))
    }

    @MainActor @Test func navigationWarningFrameAppearsWhenTriggered() throws {
        let expected = NSColor(srgbRed: 0.12, green: 0.73, blue: 0.41, alpha: 1)
        let surface = Ghostty.OSSurfaceView(id: nil, frame: .zero)
        let view = NSHostingView(rootView:
            NavigationWarningTestView(surface: surface)
                .ghosttyAccentColor(Color(nsColor: expected)))
        view.frame = NSRect(x: 0, y: 0, width: 20, height: 20)
        view.layoutSubtreeIfNeeded()
        surface.showNavigationWarning(.panel)
        // Render before yielding to other tests, which can outlast the flash.
        view.needsLayout = true
        view.layoutSubtreeIfNeeded()

        let image = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: image)
        let actual = try #require(image.colorAt(x: 1, y: 10)?.usingColorSpace(.sRGB))

        #expect(abs(actual.redComponent - expected.redComponent) < 0.005)
        #expect(abs(actual.greenComponent - expected.greenComponent) < 0.005)
        #expect(abs(actual.blueComponent - expected.blueComponent) < 0.005)
    }

    @MainActor @Test func navigationWarningCanBeReplacedAndCleared() throws {
        let surface = Ghostty.OSSurfaceView(id: nil, frame: .zero)

        surface.showNavigationWarning(.panel)
        let first = try #require(surface.navigationWarning)
        #expect(first.scope == .panel)

        surface.showNavigationWarning(.quadrant)
        let second = try #require(surface.navigationWarning)
        #expect(second.scope == .quadrant)
        #expect(second.startedAt >= first.startedAt)

        surface.clearNavigationWarning()
        #expect(surface.navigationWarning == nil)
    }

    @MainActor @Test func notificationAttentionRequiresAnUnfocusedSurface() {
        let surface = Ghostty.OSSurfaceView(id: nil, frame: .zero)

        surface.requestNotificationAttention(isFocused: true)
        #expect(!surface.notificationAttention)

        surface.requestNotificationAttention(isFocused: false)
        #expect(surface.notificationAttention)

        surface.clearNotificationAttention()
        #expect(!surface.notificationAttention)
    }
}

private struct NavigationWarningTestView: View {
    @ObservedObject var surface: Ghostty.OSSurfaceView

    var body: some View {
        Ghostty.NavigationWarningOverlay(
            warning: surface.navigationWarning,
            scope: .panel)
    }
}
