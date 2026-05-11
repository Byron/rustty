@testable import Ghostty
import AppKit
import Combine
import Testing

struct SurfaceViewAppKitTests {
    // ponytail: retain test surfaces until the test host exits; remove this
    // when surface disposal stops callbacks before releasing the view's userdata.
    @MainActor private static var retainedSurfaces: [Ghostty.SurfaceView] = []

    @MainActor
    @Test func reportedActivityUsesLiveReportsAndResetsAfterCommands() throws {
        let app = try #require((NSApp.delegate as? AppDelegate)?.ghostty.app)
        var config = Ghostty.SurfaceConfiguration()
        config.command = "/usr/bin/true"
        config.waitAfterCommand = true
        let surface = Ghostty.SurfaceView(app, baseConfig: config)
        Self.retainedSurfaces.append(surface)

        var updates: [Bool] = []
        let subscription = surface.$reportedActivity.sink { updates.append($0) }
        defer { subscription.cancel() }

        surface.commandDidStart()
        #expect(!surface.reportedActivity)
        surface.terminalTitleDidChange("⠋ Working | project")
        surface.terminalTitleDidChange("⠙ Working | project")
        surface.progressReport = .init(state: .set, progress: 50)
        #expect(updates == [false, true])

        surface.terminalTitleDidChange("[ ! ] Action Required | project")
        surface.terminalTitleDidChange("[ . ] Action Required | project")
        #expect(updates == [false, true, false])
        surface.terminalTitleDidChange("⠹ Working | project")
        surface.commandDidFinish()
        #expect(updates == [false, true, false, true, false])

        // Finishing clears activity without changing the existing progress UI.
        #expect(surface.progressReport != nil)
        surface.commandDidStart()
        #expect(!surface.reportedActivity)
        surface.progressReport = .init(state: .indeterminate, progress: nil)
        #expect(surface.reportedActivity)
        surface.progressReport = nil
        #expect(!surface.reportedActivity)
    }

    @MainActor
    @Test func restoredAndPinnedTitlesDoNotMaskLiveActivity() throws {
        let data = try JSONSerialization.data(withJSONObject: [
            "uuid": UUID().uuidString,
            "pwd": "/tmp",
            "title": "⠋ Pinned title",
            "isUserSetTitle": true,
        ])
        let surface = try JSONDecoder().decode(Ghostty.SurfaceView.self, from: data)
        Self.retainedSurfaces.append(surface)
        #expect(!surface.reportedActivity)

        surface.terminalTitleDidChange("⠙ Actual work | project")
        #expect(surface.reportedActivity)
        #expect(surface.title == "⠋ Pinned title")
        surface.terminalTitleDidChange("Actual work | project")
        #expect(!surface.reportedActivity)
        #expect(surface.title == "⠋ Pinned title")
    }

    @MainActor
    @Test func activityBadgesTrackFortyPanesAcrossBackgroundTabs() async throws {
        let ghostty = try #require(NSApp.delegate as? AppDelegate).ghostty
        let app = try #require(ghostty.app)
        var config = Ghostty.SurfaceConfiguration()
        config.command = "/usr/bin/true"
        config.waitAfterCommand = true
        let panes = (0..<40).map { _ in Ghostty.SurfaceView(app, baseConfig: config) }
        Self.retainedSurfaces.append(contentsOf: panes)
        var controllers: [BaseTerminalController] = []
        var windows: [TerminalWindow] = []
        var badges: [NSTextField] = []
        let tabbingIdentifier = UUID().uuidString
        defer {
            controllers.forEach { $0.window = nil }
            windows.forEach { $0.close() }
        }
        for tab in 0..<4 {
            let first = panes[tab * 10]
            var tree = SplitTree(view: first)
            for pane in panes[(tab * 10 + 1)..<(tab * 10 + 10)] {
                tree = try tree.inserting(view: pane, at: first, direction: .right)
            }
            let controller = BaseTerminalController(ghostty, surfaceTree: tree)
            let window = TerminalWindow(
                contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
                styleMask: [.titled, .closable],
                backing: .buffered,
                defer: true)
            window.isReleasedWhenClosed = false
            controller.window = window
            window.awakeFromNib()
            window.tabbingIdentifier = tabbingIdentifier
            controllers.append(controller)
            windows.append(window)
            let accessory = try #require(window.tab.accessoryView as? NSStackView)
            let badge = try #require(accessory.arrangedSubviews.compactMap { $0 as? NSTextField }
                .first { $0.accessibilityIdentifier() == "TabActivityCount" })
            badges.append(badge)
        }
        for window in windows.dropFirst() {
            windows[0].addTabbedWindow(window, ordered: .above)
        }
        #expect(windows[0].tabGroup?.windows.count == 4)

        // Wait for the fixture commands to exit before injecting live reports.
        for _ in 0..<200 where panes.contains(where: { !$0.processExited }) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(panes.allSatisfy { $0.processExited })

        func expectCounts(_ counts: [Int]) async throws {
            for _ in 0..<200 {
                if zip(badges, counts).allSatisfy({ $0.stringValue == "▶ \($1)" }) { break }
                try await Task.sleep(for: .milliseconds(10))
            }
            for (badge, count) in zip(badges, counts) {
                #expect(badge.stringValue == "▶ \(count)")
                #expect(badge.isHidden == (count == 0))
                #expect(badge.toolTip == (count == 1 ? "1 active pane" : "\(count) active panes"))
            }
        }

        for pane in panes { pane.commandDidStart() }
        try await expectCounts([0, 0, 0, 0])
        for pane in panes[0..<10] { pane.terminalTitleDidChange("⠋ Working | project") }
        for pane in panes[10..<14] { pane.progressReport = .init(state: .set, progress: 50) }
        panes[20].terminalTitleDidChange("⠋ Working | project")
        panes[20].progressReport = .init(state: .indeterminate, progress: nil)
        try await expectCounts([10, 4, 1, 0])

        // Switch to the idle tab, then update panes hidden by another tab's zoom.
        windows[0].tabGroup?.selectedWindow = windows[3]
        controllers[0].surfaceTree = .init(root: controllers[0].surfaceTree.root, zoomed: .leaf(view: panes[0]))
        #expect(panes[1].window == nil)
        for pane in panes[1..<10] { pane.terminalTitleDidChange("[ ! ] Action Required | project") }
        panes[10].progressReport = .init(state: .pause, progress: 50)
        panes[11].progressReport = .init(state: .error, progress: 50)
        controllers[0].titleOverride = String(repeating: "A long tab name ", count: 20)
        try await expectCounts([1, 2, 1, 0])

        // Removing/replacing a tree must stop observing its former panes.
        controllers[0].surfaceTree = .init()
        panes[0].terminalTitleDidChange("[ . ] Action Required | project")
        panes[0].terminalTitleDidChange("⠙ Still working | project")
        try await expectCounts([0, 2, 1, 0])
        controllers[0].surfaceTree = .init(view: panes[1])
        panes[1].terminalTitleDidChange("⠹ New work | project")
        try await expectCounts([1, 2, 1, 0])
        panes[1].commandDidFinish()
        try await expectCounts([0, 2, 1, 0])
    }

    @Test(arguments: [
        ("\u{0008}", true),
        ("\u{001F}", true),
        ("\u{007F}", false),
        (" ", false),
        ("h", false),
        ("", false),
        ("\u{0009}x", false),
        ("\u{0009}\u{0009}", false),
    ])
    func suppressesOnlySingleC0ControlTextWhileComposing(
        text: String,
        expected: Bool
    ) {
        #expect(
            Ghostty.SurfaceView.shouldSuppressComposingControlInput(
                text,
                composing: true
            ) == expected
        )
    }

    @Test func doesNotSuppressControlTextWhenNotComposing() {
        #expect(
            Ghostty.SurfaceView.shouldSuppressComposingControlInput(
                "\u{0008}",
                composing: false
            ) == false
        )
    }

    @Test func doesNotSuppressMissingText() {
        #expect(
            Ghostty.SurfaceView.shouldSuppressComposingControlInput(
                nil,
                composing: true
            ) == false
        )
    }
}
