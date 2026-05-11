import AppKit
import Testing
@testable import Ghostty

struct TerminalPresentationTests {
    @MainActor
    @Test func presentationSelectsOwningWindowBeforeReturningAndRevealsQuadrant() throws {
        let ghostty = try #require(NSApp.delegate as? AppDelegate).ghostty
        let app = try #require(ghostty.app)
        var config = Ghostty.SurfaceConfiguration()
        config.command = "/usr/bin/true"
        config.waitAfterCommand = true
        let topLeft = Ghostty.SurfaceView(app, baseConfig: config)
        let topRight = Ghostty.SurfaceView(app, baseConfig: config)
        let bottomLeft = Ghostty.SurfaceView(app, baseConfig: config)
        let target = Ghostty.SurfaceView(app, baseConfig: config)
        var tree = SplitTree(view: topLeft)
        tree = try tree.inserting(view: topRight, at: topLeft, direction: .right)
        tree = try tree.inserting(view: bottomLeft, at: topLeft, direction: .down)
        tree = try tree.inserting(view: target, at: topRight, direction: .down)
        let sourceQuadrant = try #require(tree.quadrant(containing: .leaf(view: topLeft)))
        let targetQuadrant = try #require(tree.quadrant(containing: .leaf(view: target)))
        let controller = BaseTerminalController(ghostty, surfaceTree: SplitTree(
            root: tree.root,
            zoomed: sourceQuadrant,
            quadrantZoomed: sourceQuadrant))
        let window = PresentationWindow(contentRect: .zero, styleMask: .titled, backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        controller.window = window
        let otherController = BaseTerminalController(ghostty, surfaceTree: .init())
        let otherWindow = PresentationWindow(contentRect: .zero, styleMask: .titled, backing: .buffered, defer: true)
        otherWindow.isReleasedWhenClosed = false
        otherController.window = otherWindow
        defer {
            controller.window = nil
            otherController.window = nil
            window.close()
            otherWindow.close()
        }

        // Zoomed-out panes can be detached. Select their controller's window
        // during the callback, without waiting for SwiftUI to attach the pane.
        #expect(target.window == nil)
        NotificationCenter.default.post(name: Ghostty.Notification.ghosttyPresentTerminal, object: target)

        #expect(window.presentationCount == 1)
        #expect(otherWindow.presentationCount == 0)
        #expect(controller.surfaceTree.zoomed == targetQuadrant)
        #expect(controller.surfaceTree.quadrantZoomed == targetQuadrant)
    }
}

private final class PresentationWindow: NSWindow {
    var presentationCount = 0

    override func makeKeyAndOrderFront(_ sender: Any?) {
        presentationCount += 1
    }
}
