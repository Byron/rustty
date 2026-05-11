import Combine
import SwiftUI

/// A single operation within the split tree.
///
/// Rather than binding the split tree (which is immutable), any mutable operations are
/// exposed via this enum to the embedder to handle.
enum TerminalSplitOperation {
    case resize(Resize)
    case drop(Drop)
    case activateQuadrant(SplitTree<Ghostty.SurfaceView>.Node)

    struct Resize {
        let node: SplitTree<Ghostty.SurfaceView>.Node
        let ratio: Double
    }

    struct Drop {
        /// The surface being dragged.
        let payload: Ghostty.SurfaceView

        /// The surface it was dragged onto
        let destination: Ghostty.SurfaceView

        /// The zone it was dropped to determine how to split the destination.
        let zone: TerminalSplitDropZone
    }
}

struct TerminalSplitTreeView: View {
    let tree: SplitTree<Ghostty.SurfaceView>
    let isQuadrantPeek: Bool
    let showsQuadrantPeekOverlay: Bool
    let action: (TerminalSplitOperation) -> Void

    var body: some View {
        if let node = tree.zoomed ?? tree.root {
            TerminalSplitSubtreeView(
                tree: tree,
                node: node,
                isRoot: node == tree.root,
                isQuadrantPeek: isQuadrantPeek,
                showsQuadrantPeekOverlay: showsQuadrantPeekOverlay,
                action: action)
            // This is necessary because we can't rely on SwiftUI's implicit
            // structural identity to detect changes to this view. Due to
            // the tree structure of splits it could result in bad behaviors.
            // See: https://github.com/ghostty-org/ghostty/issues/7546
            .id(node.structuralIdentity)
            .environment(
                \.ghosttyWorkingDirectoryLabelsLarge,
                tree.zoomed != nil && tree.zoomed == tree.quadrantZoomed)
        }
    }
}

private struct TerminalSplitSubtreeView: View {
    @EnvironmentObject var ghostty: Ghostty.App

    let tree: SplitTree<Ghostty.SurfaceView>
    let node: SplitTree<Ghostty.SurfaceView>.Node
    var isRoot: Bool = false
    let isQuadrantPeek: Bool
    let showsQuadrantPeekOverlay: Bool
    let action: (TerminalSplitOperation) -> Void

    var body: some View {
        if tree.quadrant(containing: node) == node {
            TerminalQuadrantView(
                node: node,
                isQuadrantPeek: isQuadrantPeek,
                showsQuadrantPeekOverlay: showsQuadrantPeekOverlay,
                action: action
            ) {
                subtree
            }
        } else {
            subtree
        }
    }

    @ViewBuilder private var subtree: some View {
        switch node {
        case .leaf(let leafView):
            TerminalSplitLeaf(surfaceView: leafView, isSplit: !isRoot, action: action)

        case .split(let split):
            let splitViewDirection: SplitViewDirection = switch split.direction {
            case .horizontal: .horizontal
            case .vertical: .vertical
            }

            SplitView(
                splitViewDirection,
                .init(get: {
                    CGFloat(split.ratio)
                }, set: {
                    action(.resize(.init(node: node, ratio: $0)))
                }),
                dividerColor: ghostty.config.splitDividerColor,
                dividerSize: tree.zoomed == nil && tree.separatesQuadrants(node) ? 2 : 1,
                resizeIncrements: .init(width: 1, height: 1),
                left: {
                    TerminalSplitSubtreeView(
                        tree: tree,
                        node: split.left,
                        isQuadrantPeek: isQuadrantPeek,
                        showsQuadrantPeekOverlay: showsQuadrantPeekOverlay,
                        action: action)
                },
                right: {
                    TerminalSplitSubtreeView(
                        tree: tree,
                        node: split.right,
                        isQuadrantPeek: isQuadrantPeek,
                        showsQuadrantPeekOverlay: showsQuadrantPeekOverlay,
                        action: action)
                },
                onEqualize: {
                    guard let surface = node.leftmostLeaf().surface else { return }
                    ghostty.splitEqualize(surface: surface)
                }
            )
        }
    }
}

private struct TerminalQuadrantView<Content: View>: View {
    @EnvironmentObject private var ghostty: Ghostty.App
    @Environment(\.ghosttyAccentColor) private var accentColor
    @Environment(\.ghosttyLastFocusedSurface) private var lastFocusedSurface

    let node: SplitTree<Ghostty.SurfaceView>.Node
    let isQuadrantPeek: Bool
    let showsQuadrantPeekOverlay: Bool
    let action: (TerminalSplitOperation) -> Void
    let content: Content

    @State private var commonWorkingDirectory: String?
    @State private var notificationAttention: Bool
    @State private var activity = false
    @State private var activityFlashID = 0
    @State private var activityFlashOpacity: Double = 0
    @State private var activityIsFlashing = false
    @State private var windowFocus = true

    init(
        node: SplitTree<Ghostty.SurfaceView>.Node,
        isQuadrantPeek: Bool,
        showsQuadrantPeekOverlay: Bool,
        action: @escaping (TerminalSplitOperation) -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.node = node
        self.isQuadrantPeek = isQuadrantPeek
        self.showsQuadrantPeekOverlay = showsQuadrantPeekOverlay
        self.action = action
        self.content = content()
        self._commonWorkingDirectory = State(initialValue:
            Ghostty.WorkingDirectoryBadge.commonName(pwds: node.leaves().map(\.pwd)))
        self._notificationAttention = State(initialValue:
            node.leaves().contains { $0.notificationAttention })
    }

    private var quadrantPresentation: Ghostty.WorkingDirectoryBadge.Presentation? {
        return Ghostty.WorkingDirectoryBadge.quadrantPresentation(
            name: commonWorkingDirectory,
            isFocusedQuadrant: isFocusedQuadrant,
            windowFocus: windowFocus,
            showFocused: isQuadrantPeek)
    }

    private var pwdChanges: AnyPublisher<Void, Never> {
        Publishers.MergeMany(node.leaves().map { surface in
            surface.$pwd.map { _ in () }.eraseToAnyPublisher()
        })
        .eraseToAnyPublisher()
    }

    private var notificationAttentionChanges: AnyPublisher<Bool, Never> {
        let surfaces = node.leaves()
        return Publishers.MergeMany(surfaces.map { surface in
            surface.$notificationAttention
                .map { _ in surfaces.contains { $0.notificationAttention } }
                .eraseToAnyPublisher()
        })
        .removeDuplicates()
        .eraseToAnyPublisher()
    }

    private var activityChanges: AnyPublisher<(isActive: Bool, stopped: Bool), Never> {
        BaseTerminalController.aggregateActivityPublisher(for: node.leaves())
            .receive(on: DispatchQueue.main)
            .eraseToAnyPublisher()
    }

    private var quadrantPeekFill: Color {
        guard notificationAttention else { return ghostty.config.unfocusedSplitFill }
        return accentColor
    }

    private var isFocusedQuadrant: Bool {
        lastFocusedSurface?.value.map { node.node(view: $0) != nil } ?? false
    }

    var body: some View {
        ZStack {
            content
                .environment(
                    \.ghosttyWorkingDirectoryLabelsHidden,
                    quadrantPresentation != nil)

            if showsQuadrantPeekOverlay && !isFocusedQuadrant {
                Rectangle()
                    .fill(quadrantPeekFill)
                    .opacity(ghostty.config.quadrantPeekOpacity)
                    .allowsHitTesting(false)
            }

            if let quadrantPresentation {
                GeometryReader { geometry in
                    Ghostty.QuadrantWorkingDirectoryBadge(
                        presentation: quadrantPresentation,
                        isActive: activity,
                        flashOpacity: activityFlashOpacity,
                        windowFocus: windowFocus)
                        .frame(maxWidth: geometry.size.width * 0.75)
                        .position(
                            x: geometry.size.width / 2,
                            y: geometry.size.height / 2)
                }
            }

            if isQuadrantPeek {
                ZStack {
                    Rectangle()
                        .strokeBorder(
                            accentColor.opacity(0.8),
                            lineWidth: Ghostty.OSSurfaceView.FiniteHighlight.focus.lineWidth)

                    if let surfaceView = lastFocusedSurface?.value {
                        QuadrantNavigationWarningFrame(surfaceView: surfaceView)
                    }
                }
                .allowsHitTesting(false)
                .opacity(isFocusedQuadrant ? 1 : 0)
                .animation(nil, value: isFocusedQuadrant)
            }

            if isQuadrantPeek {
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture {
                        action(.activateQuadrant(node))
                    }
            }
        }
        .onReceive(pwdChanges) { _ in
            commonWorkingDirectory = Ghostty.WorkingDirectoryBadge.commonName(
                pwds: node.leaves().map(\.pwd))
        }
        .onReceive(notificationAttentionChanges) {
            notificationAttention = $0
        }
        .onReceive(activityChanges) { state in
            activity = state.isActive

            guard
                !activityIsFlashing,
                let presentation = quadrantPresentation,
                Ghostty.WorkingDirectoryBadge.showsActivityFlash(
                    stopped: state.stopped,
                    presentation: presentation,
                    includeFocused: true)
            else { return }

            activityIsFlashing = true
            activityFlashID += 1
        }
        .task(id: activityFlashID) {
            guard activityFlashID > 0 else { return }
            defer {
                activityFlashID = 0
                activityFlashOpacity = 0
                activityIsFlashing = false
            }

            withAnimation(.easeOut(duration: 0.2)) {
                activityFlashOpacity = 1
            }
            do {
                try await Task.sleep(for: .milliseconds(200))
            } catch {
                return
            }

            withAnimation(.easeInOut(duration: 1.0)) {
                activityFlashOpacity = 0
            }
            try? await Task.sleep(for: .seconds(1))
        }
        .onPreferenceChange(Ghostty.SurfaceWindowFocusKey.self) {
            windowFocus = $0
        }
    }
}

private struct QuadrantNavigationWarningFrame: View {
    @ObservedObject var surfaceView: Ghostty.SurfaceView

    var body: some View {
        Ghostty.NavigationWarningOverlay(
            warning: surfaceView.navigationWarning,
            scope: .quadrant)
    }
}

private struct TerminalSplitLeaf: View {
    let surfaceView: Ghostty.SurfaceView
    let isSplit: Bool
    let action: (TerminalSplitOperation) -> Void

    @State private var dropState: DropState = .idle
    @State private var isSelfDragging: Bool = false

    var body: some View {
        GeometryReader { geometry in
            Ghostty.InspectableSurface(
                surfaceView: surfaceView,
                isSplit: isSplit)
            .background {
                // If we're dragging ourself, we hide the entire drop zone. This makes
                // it so that a released drop animates back to its source properly
                // so it is a proper invalid drop zone.
                if !isSelfDragging {
                    Color.clear
                        .onDrop(of: [.ghosttySurfaceId], delegate: SplitDropDelegate(
                            dropState: $dropState,
                            viewSize: geometry.size,
                            destinationSurface: surfaceView,
                            action: action
                        ))
                }
            }
            .overlay {
                if !isSelfDragging, case .dropping(let zone) = dropState {
                    zone.overlay(in: geometry)
                        .allowsHitTesting(false)
                }
            }
            .onPreferenceChange(Ghostty.DraggingSurfaceKey.self) { value in
                isSelfDragging = value == surfaceView.id
                if isSelfDragging {
                    dropState = .idle
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Terminal pane")
        }
    }

    private enum DropState: Equatable {
        case idle
        case dropping(TerminalSplitDropZone)
    }

    private struct SplitDropDelegate: DropDelegate {
        @Binding var dropState: DropState
        let viewSize: CGSize
        let destinationSurface: Ghostty.SurfaceView
        let action: (TerminalSplitOperation) -> Void

        func validateDrop(info: DropInfo) -> Bool {
            info.hasItemsConforming(to: [.ghosttySurfaceId])
        }

        func dropEntered(info: DropInfo) {
            dropState = .dropping(.calculate(at: info.location, in: viewSize))
        }

        func dropUpdated(info: DropInfo) -> DropProposal? {
            // For some reason dropUpdated is sent after performDrop is called
            // and we don't want to reset our drop zone to show it so we have
            // to guard on the state here.
            guard case .dropping = dropState else { return DropProposal(operation: .forbidden) }
            dropState = .dropping(.calculate(at: info.location, in: viewSize))
            return DropProposal(operation: .move)
        }

        func dropExited(info: DropInfo) {
            dropState = .idle
        }

        func performDrop(info: DropInfo) -> Bool {
            let zone = TerminalSplitDropZone.calculate(at: info.location, in: viewSize)
            dropState = .idle

            // Load the dropped surface asynchronously using Transferable
            let providers = info.itemProviders(for: [.ghosttySurfaceId])
            guard let provider = providers.first else { return false }

            // Capture action before the async closure
            _ = provider.loadTransferable(type: Ghostty.SurfaceView.self) { [weak destinationSurface] result in
                switch result {
                case .success(let sourceSurface):
                    DispatchQueue.main.async {
                        // Don't allow dropping on self
                        guard let destinationSurface else { return }
                        guard sourceSurface !== destinationSurface else { return }
                        action(.drop(.init(payload: sourceSurface, destination: destinationSurface, zone: zone)))
                    }

                case .failure:
                    break
                }
            }

            return true
        }
    }
}

enum TerminalSplitDropZone: String, Equatable {
    case top
    case bottom
    case left
    case right

    /// Determines which drop zone the cursor is in based on proximity to edges.
    ///
    /// Divides the view into four triangular regions by drawing diagonals from
    /// corner to corner. The drop zone is determined by which edge the cursor
    /// is closest to, creating natural triangular hit regions for each side.
    static func calculate(at point: CGPoint, in size: CGSize) -> TerminalSplitDropZone {
        let relX = point.x / size.width
        let relY = point.y / size.height

        let distToLeft = relX
        let distToRight = 1 - relX
        let distToTop = relY
        let distToBottom = 1 - relY

        let minDist = min(distToLeft, distToRight, distToTop, distToBottom)

        if minDist == distToLeft { return .left }
        if minDist == distToRight { return .right }
        if minDist == distToTop { return .top }
        return .bottom
    }

    @ViewBuilder
    func overlay(in geometry: GeometryProxy) -> some View {
        let overlayColor = Color.accentColor.opacity(0.3)

        switch self {
        case .top:
            VStack(spacing: 0) {
                Rectangle()
                    .fill(overlayColor)
                    .frame(height: geometry.size.height / 2)
                Spacer()
            }
        case .bottom:
            VStack(spacing: 0) {
                Spacer()
                Rectangle()
                    .fill(overlayColor)
                    .frame(height: geometry.size.height / 2)
            }
        case .left:
            HStack(spacing: 0) {
                Rectangle()
                    .fill(overlayColor)
                    .frame(width: geometry.size.width / 2)
                Spacer()
            }
        case .right:
            HStack(spacing: 0) {
                Spacer()
                Rectangle()
                    .fill(overlayColor)
                    .frame(width: geometry.size.width / 2)
            }
        }
    }
}
