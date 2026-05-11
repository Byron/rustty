import Foundation
import GhosttyKit
import SwiftUI

extension Ghostty {
    class OSSurfaceView: NSView, ObservableObject {
        typealias ID = UUID

        enum FiniteHighlight: Equatable {
            case focus
            case zoom

            var lineWidth: CGFloat {
                switch self {
                case .focus: 1.5
                case .zoom: 3
                }
            }
        }

        struct NavigationWarning: Equatable, Sendable {
            enum Scope: Equatable, Sendable {
                case panel
                case quadrant
            }

            static let duration: Duration = .milliseconds(400)

            let scope: Scope
            let startedAt: Date

            func isVisible(at date: Date, reduceMotion: Bool) -> Bool {
                let elapsed = date.timeIntervalSince(startedAt)
                guard elapsed >= 0, elapsed < Self.duration.timeInterval else { return false }
                return reduceMotion || elapsed < 0.15 || elapsed >= 0.25
            }
        }

        /// Unique ID per surface
        let id: UUID

        // The current pwd of the surface as defined by the pty. This can be
        // changed with escape codes.
        @Published var pwd: String?

        // The cell size of this surface. This is set by the core when the
        // surface is first created and any time the cell size changes (i.e.
        // when the font size changes). This is used to allow windows to be
        // resized in discrete steps of a single cell.
        @Published var cellSize: CGSize = .zero

        // The health state of the surface. This currently only reflects the
        // renderer health. In the future we may want to make this an enum.
        @Published var healthy: Bool = true

        // Any error while initializing the surface.
        @Published var error: Error?

        // The hovered URL string
        @Published var hoverUrl: String?

        // The progress report (if any)
        @Published var progressReport: Action.ProgressReport?

        // The currently active key tables. Empty if no tables are active.
        @Published var keyTables: [String] = []

        // The current search state. When non-nil, the search overlay should be shown.
        @Published var searchState: SearchState?

        // The time this surface last became focused. This is a ContinuousClock.Instant
        // on supported platforms.
        @Published var focusInstant: ContinuousClock.Instant?

        // Returns sizing information for the surface. This is the raw C
        // structure because I'm lazy.
        @Published var surfaceSize: ghostty_surface_size_s?

        /// True when the surface is in readonly mode.
        @Published private(set) var readonly: Bool = false

        /// True when the surface should show a highlight effect (e.g., when presented via goto_split).
        @Published private(set) var highlighted: Bool = false

        /// Briefly identifies this surface after focus or zoom changes.
        @Published private(set) var finiteHighlight: FiniteHighlight?

        private var finiteHighlightTask: Task<Void, Never>?

        /// Briefly warns that a focus navigation action had no destination.
        @Published private(set) var navigationWarning: NavigationWarning?

        private var navigationWarningTask: Task<Void, Never>?

        /// True while an OSC desktop notification from this surface needs attention.
        @Published private(set) var notificationAttention = false

        /// A message sent from `ghostty_surface_t` when a child process exited
        @Published private(set) var childExitedMessage: ChildExitedMessage?

        var surface: ghostty_surface_t? {
            nil
        }

        init(id: UUID?, frame: CGRect) {
            self.id = id ?? UUID()
            super.init(frame: frame)

            // Before we initialize the surface we want to register our notifications
            // so there is no window where we can't receive them.
            let center = NotificationCenter.default
            center.addObserver(
                self,
                selector: #selector(ghosttyDidChangeReadonly(_:)),
                name: .ghosttyDidChangeReadonly,
                object: self,
            )
        }

        required init?(coder: NSCoder) {
            fatalError("init(coder:) is not supported for this view")
        }

        deinit {
            NotificationCenter.default
                .removeObserver(self)
        }

        @objc private func ghosttyDidChangeReadonly(_ notification: Foundation.Notification) {
            guard let value = notification.userInfo?[Foundation.Notification.Name.ReadonlyKey] as? Bool else { return }
            readonly = value
        }

        /// Triggers a brief highlight animation on this surface.
        func highlight() {
            highlighted = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
                self?.highlighted = false
            }
        }

        /// Briefly identifies this surface after a multi-pane zoom change.
        func highlightZoom() {
            highlightFinite(.zoom)
        }

        /// Briefly identifies this surface after keyboard focus navigation.
        func highlightFocus() {
            highlightFinite(.focus)
        }

        private func highlightFinite(_ highlight: FiniteHighlight) {
            clearNavigationWarning()
            finiteHighlightTask?.cancel()
            finiteHighlight = highlight
            finiteHighlightTask = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(200))
                guard !Task.isCancelled else { return }
                self?.finiteHighlight = nil
            }
        }

        func showNavigationWarning(_ scope: NavigationWarning.Scope) {
            finiteHighlightTask?.cancel()
            finiteHighlight = nil
            navigationWarningTask?.cancel()

            let warning = NavigationWarning(scope: scope, startedAt: Date())
            navigationWarning = warning
            navigationWarningTask = Task { @MainActor [weak self] in
                try? await Task.sleep(for: NavigationWarning.duration)
                guard !Task.isCancelled, self?.navigationWarning == warning else { return }
                self?.navigationWarning = nil
            }
        }

        func clearNavigationWarning() {
            navigationWarningTask?.cancel()
            navigationWarningTask = nil
            navigationWarning = nil
        }

        func requestNotificationAttention(isFocused: Bool) {
            guard !isFocused else { return }
            notificationAttention = true
        }

        func clearNotificationAttention() {
            notificationAttention = false
        }

        func setChildExitedMessage(_ message: ChildExitedMessage) {
            self.childExitedMessage = message
        }

        @MainActor
        func endSearch() {
            searchState = nil
        }

        // MARK: - Placeholders

        func focusDidChange(_ focused: Bool) {}

        func sizeDidChange(_ size: CGSize) {}
    }
}

// MARK: Search State

extension Ghostty.OSSurfaceView {
    @MainActor class SearchState: ObservableObject {

        /// We should always change needle's text and its selection together
        struct Needle: Equatable {
            var text: String
            var selection: Range<String.Index>?

            static let empty = Needle(text: "", selection: nil)
        }

        /// The pasteboard used to persist the search needle.
        ///
        /// The `.find` pasteboard lets us sync our needle across the system and other find bars.
        private let pasteboard: NSPasteboard

        @Published var needle = Needle.empty

        @Published var selected: UInt?
        @Published var total: UInt?

        init(
            from startSearch: Ghostty.Action.StartSearch,
            pasteboard: NSPasteboard? = nil
        ) {
            self.pasteboard = pasteboard ?? .find
            if let needle = startSearch.needle, !needle.isEmpty {
                setNeedle(needle)
                writePasteboardNeedle()
            } else {
                readPasteboardNeedle()
            }
        }

        /// Replaces the search needle while keeping its selection valid.
        func setNeedle(_ needle: String, selectAll: Bool = false) {
            self.needle = .init(
                text: needle,
                selection: selectAll ? needle.startIndex..<needle.endIndex : nil
            )
        }

        func readPasteboardNeedle() {
            let pasteboardNeedle = pasteboard.string
            if let pasteboardNeedle, pasteboardNeedle != needle.text {
                setNeedle(pasteboardNeedle, selectAll: true)
            }
        }

        func writePasteboardNeedle() {
            pasteboard.string = needle.text
        }
    }

    func navigateSearchToNext() -> Bool {
        guard let surface = self.surface else { return false }
        let action = "navigate_search:next"
        if !ghostty_surface_binding_action(surface, action, UInt(action.lengthOfBytes(using: .utf8))) {
            AppDelegate.logger.warning("action failed action=\(action, privacy: .public)")
            return false
        }
        return true
    }

    func navigateSearchToPrevious() -> Bool {
        guard let surface = self.surface else { return false }
        let action = "navigate_search:previous"
        if !ghostty_surface_binding_action(surface, action, UInt(action.lengthOfBytes(using: .utf8))) {
            AppDelegate.logger.warning("action failed action=\(action, privacy: .public)")
            return false
        }
        return true
    }
}
