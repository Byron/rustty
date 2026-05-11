import AppKit
import Testing
@testable import Ghostty

struct ZoomedTabTintTests {
    @Test func tabAccentUsesSelectedColor() throws {
        try expectColor(TerminalTabColor.red.accentColor, equals: .systemRed)
    }

    @Test func tabAccentFallsBackToSystemAccent() throws {
        try expectColor(TerminalTabColor.none.accentColor, equals: .controlAccentColor)
    }

    @Test func tabAccentForegroundContrastsWithAccent() throws {
        try expectColor(TerminalTabColor.yellow.accentForegroundColor, equals: .black)
        try expectColor(TerminalTabColor.red.accentForegroundColor, equals: .white)
    }

    @Test func noTabColorUsesIconColor() throws {
        let iconColor = NSColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1)
        let tint = ZoomedTabTint.make(
            iconColor: iconColor,
            tabColor: .none,
            opacity: 0.25)

        try expectColor(tint.color, equals: iconColor)
        #expect(abs(tint.opacity - 0.25) < 0.001)
    }

    @Test func tabColorBlendsWithIconColor() throws {
        let iconColor = NSColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1)
        let tabColor = try #require(TerminalTabColor.red.displayColor?.usingColorSpace(.sRGB))
        let tint = ZoomedTabTint.make(
            iconColor: iconColor,
            tabColor: .red,
            opacity: 0.3)

        let expected = NSColor(
            red: iconColor.redComponent * 0.65 + tabColor.redComponent * 0.35,
            green: iconColor.greenComponent * 0.65 + tabColor.greenComponent * 0.35,
            blue: iconColor.blueComponent * 0.65 + tabColor.blueComponent * 0.35,
            alpha: 1)

        try expectColor(tint.color, equals: expected)
        #expect(abs(tint.opacity - 0.3) < 0.001)
    }

    @Test func opacityCanBeZero() {
        let tint = ZoomedTabTint.make(
            iconColor: .controlAccentColor,
            tabColor: .none,
            opacity: 0)

        #expect(tint.opacity == 0)
    }

    @Test func zoomStateDistinguishesPanelZoom() throws {
        let (tree, view1, _) = try SplitTreeTests.makeHorizontalSplit()
        let zoomed = try #require(tree.root?.node(view: view1))

        let zoomedTree = SplitTree(root: tree.root, zoomed: zoomed, quadrantZoomed: nil)

        #expect(SurfaceZoomState.from(zoomedTree) == .panel)
    }

    @Test func zoomStateDistinguishesQuadrantZoom() throws {
        let (tree, view1, _) = try makeQuadrantTree()
        let targetNode = try #require(tree.root?.node(view: view1))
        let quadrant = try #require(tree.quadrant(containing: targetNode))

        let zoomedTree = SplitTree(root: tree.root, zoomed: quadrant, quadrantZoomed: quadrant)

        #expect(SurfaceZoomState.from(zoomedTree) == .quadrant)
    }

    @Test func zoomStateDistinguishesPanelZoomInsideQuadrant() throws {
        let (tree, view1, _) = try makeQuadrantTree()
        let targetNode = try #require(tree.root?.node(view: view1))
        let quadrant = try #require(tree.quadrant(containing: targetNode))

        let zoomedTree = SplitTree(root: tree.root, zoomed: targetNode, quadrantZoomed: quadrant)

        #expect(SurfaceZoomState.from(zoomedTree) == .panelInQuadrant)
    }

    @Test func exclusiveZoomTargetDetection() throws {
        let (tree, view1, _) = try makeQuadrantTree()
        let targetNode = try #require(tree.root?.node(view: view1))
        let quadrant = try #require(tree.quadrant(containing: targetNode))

        let panelZoom = SplitTree(root: tree.root, zoomed: targetNode, quadrantZoomed: quadrant)
        let quadrantZoom = SplitTree(root: tree.root, zoomed: quadrant, quadrantZoomed: quadrant)
        let singleView = try #require(tree.root?.rightmostLeaf())
        let singleTarget = try #require(tree.root?.node(view: singleView))
        let singleQuadrant = try #require(tree.quadrant(containing: singleTarget))
        let singleQuadrantZoom = SplitTree(
            root: tree.root,
            zoomed: singleQuadrant,
            quadrantZoomed: singleQuadrant)

        #expect(panelZoom.isExclusivelyShowing(targetNode))
        #expect(!quadrantZoom.isExclusivelyShowing(targetNode))
        #expect(singleQuadrant == singleTarget)
        #expect(singleQuadrantZoom.isExclusivelyShowing(singleTarget))
        #expect(!tree.isExclusivelyShowing(targetNode))
    }

    private func expectColor(
        _ actual: NSColor,
        equals expected: NSColor,
        tolerance: CGFloat = 0.001
    ) throws {
        let actual = try #require(actual.usingColorSpace(.sRGB))
        let expected = try #require(expected.usingColorSpace(.sRGB))

        #expect(abs(actual.redComponent - expected.redComponent) < tolerance)
        #expect(abs(actual.greenComponent - expected.greenComponent) < tolerance)
        #expect(abs(actual.blueComponent - expected.blueComponent) < tolerance)
    }

    private func makeQuadrantTree() throws -> (SplitTree<MockView>, MockView, MockView) {
        let view1 = MockView()
        let view2 = MockView()
        let view3 = MockView()
        let view4 = MockView()
        let view5 = MockView()
        var tree = SplitTree<MockView>(view: view1)
        tree = try tree.inserting(view: view2, at: view1, direction: .right)
        tree = try tree.inserting(view: view3, at: view1, direction: .down)
        tree = try tree.inserting(view: view4, at: view2, direction: .down)
        tree = try tree.inserting(view: view5, at: view1, direction: .right)
        return (tree, view1, view5)
    }
}
