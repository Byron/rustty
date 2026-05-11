import AppKit
import Testing
@testable import Ghostty

@Suite
struct QuadrantSwitchTests {
    @Test func blockedNavigationEligibility() {
        let modifiers: NSEvent.ModifierFlags = [.command, .control]

        #expect(BaseTerminalController.shouldHandleBlockedQuadrantNavigation(
            hasQuadrantZoom: true,
            isSwitching: false,
            modifiers: modifiers))
        #expect(BaseTerminalController.shouldHandleBlockedQuadrantNavigation(
            hasQuadrantZoom: false,
            isSwitching: true,
            modifiers: modifiers))
        #expect(!BaseTerminalController.shouldHandleBlockedQuadrantNavigation(
            hasQuadrantZoom: false,
            isSwitching: false,
            modifiers: modifiers))
        #expect(!BaseTerminalController.shouldHandleBlockedQuadrantNavigation(
            hasQuadrantZoom: true,
            isSwitching: false,
            modifiers: []))
    }

    @Test func blockedPanelNavigationOnlyKeepsSplitQuadrantZoomed() throws {
        let (tree, first, _) = try SplitTreeTests.makeHorizontalSplit()
        let target = try #require(tree.root?.node(view: first))
        let quadrant = try #require(tree.root)

        #expect(!BaseTerminalController.shouldUnzoomAfterBlockedPanelNavigation(tree))
        #expect(BaseTerminalController.shouldUnzoomAfterBlockedPanelNavigation(
            SplitTree(root: tree.root, zoomed: target)))
        #expect(BaseTerminalController.shouldUnzoomAfterBlockedPanelNavigation(
            SplitTree(root: tree.root, zoomed: target, quadrantZoomed: quadrant)))
        #expect(!BaseTerminalController.shouldUnzoomAfterBlockedPanelNavigation(
            SplitTree(root: tree.root, zoomed: quadrant, quadrantZoomed: quadrant)))

        let singlePanel = SplitTree<MockView>(view: MockView())
        let leaf = try #require(singlePanel.root)
        #expect(BaseTerminalController.shouldUnzoomAfterBlockedPanelNavigation(
            SplitTree(root: leaf, zoomed: leaf, quadrantZoomed: leaf)))
    }

    @Test func blockedNavigationPreservesDestination() {
        var state = BaseTerminalController.QuadrantSwitch(
            modifiers: NSEvent.ModifierFlags([.command, .control]),
            target: "original",
            fullZoomTarget: "zoomed")

        state.updateTarget(nil)
        #expect(state.target == "original")

        state.updateTarget("next")
        #expect(state.target == "next")
        #expect(state.fullZoomTarget == "zoomed")
    }

    @Test func navigationHidesContrastOverlay() {
        var state = BaseTerminalController.QuadrantSwitch(
            modifiers: NSEvent.ModifierFlags([.command, .control]),
            target: "original")

        #expect(state.showsContrastOverlay)
        state.updateTarget("next")
        #expect(state.showsContrastOverlay)
        state.markNavigationKeyUsed()
        #expect(!state.showsContrastOverlay)
        #expect(BaseTerminalController.QuadrantSwitch(
            modifiers: NSEvent.ModifierFlags([.command, .control]),
            target: "new").showsContrastOverlay)
    }

    @Test func clickRestoresRememberedPaneWithinQuadrant() throws {
        let (tree, first, second) = try SplitTreeTests.makeHorizontalSplit()
        let quadrant = try #require(tree.root)

        #expect(BaseTerminalController.quadrantActivationTarget(
            in: quadrant,
            remembered: second) === second)
        #expect(BaseTerminalController.quadrantActivationTarget(
            in: quadrant,
            remembered: MockView()) === first)
    }

    @Test func commitsWhenInitiatingChordBreaks() {
        let initiating: NSEvent.ModifierFlags = [.command, .control]

        #expect(!BaseTerminalController.shouldCommitQuadrantSwitch(
            initiating: initiating,
            current: [.command, .control]))
        #expect(!BaseTerminalController.shouldCommitQuadrantSwitch(
            initiating: initiating,
            current: [.command, .control, .shift]))
        #expect(BaseTerminalController.shouldCommitQuadrantSwitch(
            initiating: initiating,
            current: [.command]))
        #expect(BaseTerminalController.shouldCommitQuadrantSwitch(
            initiating: initiating,
            current: [.control]))
    }

    @Test func peekBeginsWhenConfiguredChordIsComplete() {
        let configured: [NSEvent.ModifierFlags] = [
            [.command, .control],
            [.command, .option],
        ]

        #expect(BaseTerminalController.shouldBeginQuadrantPeek(
            modifiers: [.command, .control],
            configured: configured,
            hasQuadrantZoom: true))
        #expect(!BaseTerminalController.shouldBeginQuadrantPeek(
            modifiers: [.command],
            configured: configured,
            hasQuadrantZoom: true))
        #expect(!BaseTerminalController.shouldBeginQuadrantPeek(
            modifiers: [.command, .control, .shift],
            configured: configured,
            hasQuadrantZoom: true))
        #expect(!BaseTerminalController.shouldBeginQuadrantPeek(
            modifiers: [.command, .control],
            configured: configured,
            hasQuadrantZoom: false))
    }
}
