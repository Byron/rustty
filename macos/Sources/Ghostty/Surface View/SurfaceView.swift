import SwiftUI
import UserNotifications
import GhosttyKit
import System

extension Ghostty {
    struct SurfaceWrapper: View {
        // The surface to create a view for. This must be created upstream. As long as this
        // remains the same, the surface that is being rendered remains the same.
        @ObservedObject var surfaceView: SurfaceView

        // True if this surface is part of a split view. This is important to know so
        // we know whether to dim the surface out of focus.
        var isSplit: Bool = false

        // Maintain whether our view has focus or not
        @FocusState private var surfaceFocus: Bool

        // Maintain whether our window has focus (is key) or not
        @State private var windowFocus: Bool = true

        // Briefly show the working directory when this surface gains focus.
        @State private var showFocusedWorkingDirectory: Bool = false
        @State private var workingDirectoryPosition: CGPoint?

        // Activity presentation for this individual pane.
        @State private var paneActivity: Bool = false
        @State private var paneActivityTransition = BaseTerminalController.ActivityTransition()
        @State private var paneActivityFlashID: Int = 0
        @State private var paneActivityFlashOpacity: Double = 0
        @State private var paneActivityIsFlashing: Bool = false

        // Observe SecureInput to detect when its enabled
        @ObservedObject private var secureInput = SecureInput.shared

        @EnvironmentObject private var ghostty: Ghostty.App
        @Environment(\.ghosttyLastFocusedSurface) private var lastFocusedSurface
        @Environment(\.ghosttyWorkingDirectoryLabelsHidden) private var workingDirectoryLabelsHidden
        @Environment(\.ghosttyWorkingDirectoryLabelsLarge) private var workingDirectoryLabelsLarge

        private var isFocusedSurface: Bool {
            surfaceFocus || lastFocusedSurface?.value === surfaceView
        }

        private var workingDirectoryPresentation: WorkingDirectoryBadge.Presentation? {
            guard !workingDirectoryLabelsHidden else { return nil }
            return WorkingDirectoryBadge.presentation(
                pwd: surfaceView.pwd,
                isFocusedSurface: isFocusedSurface,
                windowFocus: windowFocus,
                showFocusedSurface: showFocusedWorkingDirectory
            )
        }
        var body: some View {
            let center = NotificationCenter.default

            ZStack {
                // We use a GeometryReader to get the frame bounds so that our metal surface
                // is up to date. See TerminalSurfaceView for why we don't use the NSView
                // resize callback.
                GeometryReader { geo in
                    let pubBecomeKey = center.publisher(for: NSWindow.didBecomeKeyNotification)
                    let pubResign = center.publisher(for: NSWindow.didResignKeyNotification)

                    SurfaceRepresentable(view: surfaceView, size: geo.size)
                        .focused($surfaceFocus)
                        .focusedValue(\.ghosttySurfacePwd, surfaceView.pwd)
                        .focusedValue(\.ghosttySurfaceView, surfaceView)
                        .focusedValue(\.ghosttySurfaceCellSize, surfaceView.cellSize)
                        .onAppear {
                            windowFocus = surfaceView.window?.isKeyWindow ?? true
                        }
                        .onReceive(pubBecomeKey) { notification in
                            guard let window = notification.object as? NSWindow else { return }
                            guard let surfaceWindow = surfaceView.window else { return }
                            windowFocus = surfaceWindow == window
                        }
                        .onReceive(pubResign) { notification in
                            guard let window = notification.object as? NSWindow else { return }
                            guard let surfaceWindow = surfaceView.window else { return }
                            if surfaceWindow == window {
                                windowFocus = false
                            }
                        }

                    // If our geo size changed then we show the resize overlay as configured.
                    if let surfaceSize = surfaceView.surfaceSize {
                        SurfaceResizeOverlay(
                            geoSize: geo.size,
                            size: surfaceSize,
                            overlay: ghostty.config.resizeOverlay,
                            position: ghostty.config.resizeOverlayPosition,
                            duration: ghostty.config.resizeOverlayDuration,
                            focusInstant: surfaceView.focusInstant)

                    }
                }
                .ghosttySurfaceView(surfaceView)

                // Progress report
                if let progressReport = surfaceView.progressReport,
                   progressReport.state != .remove,
                   surfaceView.isWindowVisible == true {
                    VStack(spacing: 0) {
                        SurfaceProgressBar(report: progressReport)
                        Spacer()
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                    .allowsHitTesting(false)
                    .transition(.opacity)
                }

                SurfaceStatusBadges(
                    readonly: surfaceView.readonly,
                    workingDirectory: workingDirectoryPresentation,
                    workingDirectoryPosition: workingDirectoryPosition,
                    largeWorkingDirectoryLabels: workingDirectoryLabelsLarge,
                    windowFocus: windowFocus,
                    workingDirectoryActivity: paneActivity,
                    workingDirectoryFlashOpacity: paneActivityFlashOpacity,
                    onDisableReadonly: {
                        surfaceView.toggleReadonly(nil)
                    }
                )
                .zIndex(1)
                .onReceive(surfaceView.activityPublisher) { isActive in
                    let stopped = paneActivityTransition.stopped(isActive)
                    paneActivity = isActive

                    guard
                        !paneActivityIsFlashing,
                        let presentation = workingDirectoryPresentation,
                        WorkingDirectoryBadge.showsActivityFlash(
                            stopped: stopped,
                            presentation: presentation)
                    else { return }

                    paneActivityIsFlashing = true
                    paneActivityFlashID += 1
                }
                .task(id: paneActivityFlashID) {
                    guard paneActivityFlashID > 0 else { return }
                    defer {
                        paneActivityFlashID = 0
                        paneActivityFlashOpacity = 0
                        paneActivityIsFlashing = false
                    }

                    withAnimation(.easeOut(duration: 0.2)) {
                        paneActivityFlashOpacity = 1
                    }
                    do {
                        try await Task.sleep(for: .milliseconds(200))
                    } catch {
                        return
                    }

                    withAnimation(.easeInOut(duration: 1.0)) {
                        paneActivityFlashOpacity = 0
                    }
                    try? await Task.sleep(for: .seconds(1))
                }
                .task(id: isFocusedSurface && windowFocus) {
                    let presentation = WorkingDirectoryBadge.presentation(
                        pwd: surfaceView.pwd,
                        isFocusedSurface: isFocusedSurface,
                        windowFocus: windowFocus,
                        showFocusedSurface: true
                    )
                    let labelWidth = presentation.map {
                        WorkingDirectoryBadge.width(for: $0)
                    }
                    let initialPosition = labelWidth.flatMap {
                        surfaceView.workingDirectoryLabelPosition(labelWidth: $0)
                    }
                    workingDirectoryPosition = initialPosition

                    guard isFocusedSurface && windowFocus else {
                        showFocusedWorkingDirectory = false
                        return
                    }

                    withAnimation(.easeIn(duration: 0.15)) {
                        showFocusedWorkingDirectory = true
                    }
                    do {
                        if let initialPosition, let labelWidth {
                            if try await WorkingDirectoryBadge.cursorMoved(
                                from: initialPosition,
                                currentPosition: {
                                    surfaceView.workingDirectoryLabelPosition(labelWidth: labelWidth)
                                }
                            ) {
                                showFocusedWorkingDirectory = false
                                return
                            }
                        } else {
                            try await Task.sleep(for: .seconds(2))
                        }
                    } catch {
                        return
                    }

                    withAnimation(.easeOut(duration: 0.3)) {
                        showFocusedWorkingDirectory = false
                    }
                }

                // Show key state indicator for active key tables and/or pending key sequences
                KeyStateIndicator(
                    keyTables: surfaceView.keyTables,
                    keySequence: surfaceView.keySequence
                )
                .zIndex(1)

                VStack(spacing: 0) {
                    // If we have a URL from hovering a link, we show that.
                    if let url = surfaceView.hoverUrl {
                        URLHoverBanner(url: url)
                    }

                    // Show a bar to indicate a child process has exited.
                    if let msg = surfaceView.childExitedMessage {
                        ChildExitedMessageBar(msg: msg)
                            .font(.system(size: min(surfaceView.cellSize.height * 0.8, 30)))
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)

                // If we have secure input enabled and we're the focused surface and window
                // then we want to show the secure input overlay.
                if ghostty.config.secureInputIndication &&
                    secureInput.enabled &&
                    surfaceFocus &&
                    windowFocus {
                    SecureInputOverlay()
                }

                // Search overlay
                if let searchState = surfaceView.searchState {
                    SurfaceSearchOverlay(
                        surfaceView: surfaceView,
                        searchState: searchState,
                        onClose: {
                            surfaceView.endSearch()
                        }
                    )
                }

                // Show bell border if enabled
                if ghostty.config.bellFeatures.contains(.border) {
                    BellBorderOverlay(bell: surfaceView.bell)
                }

                // Show a highlight effect when this surface needs attention
                HighlightOverlay(highlighted: surfaceView.highlighted)

                NotificationAttentionOverlay(active: surfaceView.notificationAttention)

                // Identify the focused surface after focus or zoom changes.
                FiniteHighlightOverlay(highlight: surfaceView.finiteHighlight)

                NavigationWarningOverlay(
                    warning: surfaceView.navigationWarning,
                    scope: .panel)

                // If our surface is not healthy, then we render an error view over it.
                if !surfaceView.healthy {
                    Rectangle().fill(ghostty.config.backgroundColor)
                    SurfaceRendererUnhealthyView()
                } else if surfaceView.error != nil {
                    Rectangle().fill(ghostty.config.backgroundColor)
                    SurfaceErrorView()
                }

                // If we're part of a split view and don't have focus, we put a semi-transparent
                // rectangle above our view to make it look unfocused. We include the last
                // focused surface so this still works while SwiftUI focus is temporarily nil.
                if isSplit && !isFocusedSurface {
                    let overlayOpacity = ghostty.config.unfocusedSplitOpacity
                    if overlayOpacity > 0 {
                        Rectangle()
                            .fill(ghostty.config.unfocusedSplitFill)
                            .allowsHitTesting(false)
                            .opacity(overlayOpacity)
                    }
                }

                // Grab handle for dragging the window. We want this to appear at the very
                // top Z-index os it isn't faded by the unfocused overlay.
                SurfaceGrabHandle(
                    surfaceView: surfaceView,
                    dragHandle: ghostty.config.dragHandle,
                )
            }
            .preference(key: SurfaceWindowFocusKey.self, value: windowFocus)
        }
    }

    struct SurfaceRendererUnhealthyView: View {
        var body: some View {
            HStack {
                Image("AppIconImage")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 128, height: 128)

                VStack(alignment: .leading) {
                    Text("Oh, no. 😭").font(.title)
                    Text("""
                        The renderer has failed. This is usually due to exhausting
                        available GPU memory. Please free up available resources.
                        """.replacingOccurrences(of: "\n", with: " ")
                    )
                    .frame(maxWidth: 350)
                }
            }
            .padding()
        }
    }

    struct SurfaceErrorView: View {
        var body: some View {
            HStack {
                Image("AppIconImage")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 128, height: 128)

                VStack(alignment: .leading) {
                    Text("Oh, no. 😭").font(.title)
                    Text("""
                        The terminal failed to initialize. Please check the logs for
                        more information. This is usually a bug.
                        """.replacingOccurrences(of: "\n", with: " ")
                    )
                    .frame(maxWidth: 350)
                }
            }
            .padding()
        }
    }

    // This is the resize overlay that shows on top of a surface to show the current
    // size during a resize operation.
    struct SurfaceResizeOverlay: View {
        let geoSize: CGSize
        let size: ghostty_surface_size_s
        let overlay: Ghostty.Config.ResizeOverlay
        let position: Ghostty.Config.ResizeOverlayPosition
        let duration: UInt
        let focusInstant: ContinuousClock.Instant?

        // This is the last size that we processed. This is how we handle our
        // timer state.
        @State var lastSize: CGSize?

        // Ready is set to true after a short delay. This avoids some of the
        // challenges of initial view sizing from SwiftUI.
        @State var ready: Bool = false

        // Fixed value set based on personal taste.
        private let padding: CGFloat = 5

        // This computed boolean is set to true when the overlay should be hidden.
        private var hidden: Bool {
            // If we aren't ready yet then we wait...
            if !ready { return true; }

            // Hidden if we already processed this size.
            if lastSize == geoSize { return true; }

            // If we were focused recently we hide it as well. This avoids showing
            // the resize overlay when SwiftUI is lazily resizing.
            if let instant = focusInstant {
                let d = instant.duration(to: ContinuousClock.now)
                if d < .milliseconds(500) {
                    // Avoid this size completely. We can't set values during
                    // view updates so we have to defer this to another tick.
                    DispatchQueue.main.async {
                        lastSize = geoSize
                    }

                    return true
                }
            }

            // Hidden depending on overlay config
            switch overlay {
            case .never: return true
            case .always: return false
            case .after_first: return lastSize == nil
            }
        }

        var body: some View {
            VStack {
                if !position.top() {
                    Spacer()
                }

                HStack {
                    if !position.left() {
                        Spacer()
                    }

                    Text(verbatim: "\(size.columns) ⨯ \(size.rows)")
                        .padding(.init(top: padding, leading: padding, bottom: padding, trailing: padding))
                        .background(
                            RoundedRectangle(cornerRadius: 4)
                                .fill(.background)
                                .shadow(radius: 3)
                        )
                        .lineLimit(1)
                        .truncationMode(.tail)

                    if !position.right() {
                        Spacer()
                    }
                }

                if !position.bottom() {
                    Spacer()
                }
            }
            .allowsHitTesting(false)
            .opacity(hidden ? 0 : 1)
            .task {
                // Sleep chosen arbitrarily... a better long term solution would be to detect
                // when the size stabilizes (coalesce a value) for the first time and then after
                // that show the resize overlay consistently.
                try? await Task.sleep(nanoseconds: 500 * 1_000_000)
                ready = true
            }
            .task(id: geoSize) {
                // By ID-ing the task on the geoSize, we get the task to restart if our
                // geoSize changes. This also ensures that future resize overlays are shown
                // properly.

                // We only sleep if we're ready. If we're not ready then we want to set
                // our last size right away to avoid a flash.
                if ready {
                    try? await Task.sleep(nanoseconds: UInt64(duration) * 1_000_000)
                }

                lastSize = geoSize
            }
        }
    }

    /// Search overlay view that displays a search bar with input field and navigation buttons.
    struct SurfaceSearchOverlay: View {
        let surfaceView: SurfaceView
        @ObservedObject var searchState: SurfaceView.SearchState
        let onClose: () -> Void
        @State private var corner: Corner = .topRight
        @State private var dragOffset: CGSize = .zero
        @State private var barSize: CGSize = .zero
        @FocusState private var isSearchFieldFocused: Bool

        private let padding: CGFloat = 8

        var body: some View {
            GeometryReader { geo in
                HStack(spacing: 4) {
                    BackportSelectionTextField(
                        "Search",
                        text: $searchState.needle.text,
                        selection: $searchState.needle.selection
                    )
                    .textFieldStyle(.plain)
                    .frame(width: 180)
                    .padding(.leading, 8)
                    .padding(.trailing, 50)
                    .padding(.vertical, 6)
                    .background(Color.primary.opacity(0.1))
                    .cornerRadius(6)
                    .focused($isSearchFieldFocused)
                    .overlay(alignment: .trailing) {
                        if let selected = searchState.selected {
                            Text("\(selected + 1)/\(searchState.total, default: "?")")
                                .font(.caption)
                                .foregroundColor(.secondary)
                                .monospacedDigit()
                                .padding(.trailing, 8)
                        } else if let total = searchState.total {
                            Text("-/\(total)")
                                .font(.caption)
                                .foregroundColor(.secondary)
                                .monospacedDigit()
                                .padding(.trailing, 8)
                        }
                    }
                    .onChange(of: searchState.needle.text) { _ in
                        searchState.writePasteboardNeedle()
                    }
                    .onReceive(
                        NotificationCenter.default.publisher(
                            for: NSApplication.didBecomeActiveNotification
                        )
                    ) { _ in
                        // When the app becomes active, we want to check for external changes
                        // to our synced needle.
                        searchState.readPasteboardNeedle()
                    }
                    .onSubmit {
                        _ = surfaceView.navigateSearchToNext()
                    }
                    .onExitCommand(perform: onClose)
                    .backport.onKeyPress(.return) { modifiers in
                        if modifiers.contains(.shift) {
                            _ = surfaceView.navigateSearchToPrevious()
                            return .handled
                        }
                        return .ignored
                    }

                    Button(action: {
                        _ = surfaceView.navigateSearchToNext()
                    }, label: {
                        Image(systemName: "chevron.up")
                    })
                    .buttonStyle(SearchButtonStyle())

                    Button(action: {
                        guard let surface = surfaceView.surface else { return }
                        let action = "navigate_search:previous"
                        ghostty_surface_binding_action(surface, action, UInt(action.lengthOfBytes(using: .utf8)))
                    }, label: {
                        Image(systemName: "chevron.down")
                    })
                    .buttonStyle(SearchButtonStyle())

                    Button(action: onClose) {
                        Image(systemName: "xmark")
                    }
                    .buttonStyle(SearchButtonStyle())
                }
                .padding(8)
                .background(.background)
                .clipShape(clipShape)
                .shadow(radius: 4)
                .opacity(isSearchFieldFocused ? 1 : 0.8)
                .onAppear {
                    isSearchFieldFocused = true
                }
                .onReceive(NotificationCenter.default.publisher(for: .ghosttySearchFocus)) { notification in
                    guard notification.object as? SurfaceView === surfaceView else { return }
                    DispatchQueue.main.async {
                        isSearchFieldFocused = true
                    }
                }
                .background(
                    GeometryReader { barGeo in
                        Color.clear.onAppear {
                            barSize = barGeo.size
                        }
                    }
                )
                .padding(padding)
                .offset(dragOffset)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: corner.alignment)
                .gesture(
                    DragGesture()
                        .onChanged { value in
                            dragOffset = value.translation
                        }
                        .onEnded { value in
                            let centerPos = centerPosition(for: corner, in: geo.size, barSize: barSize)
                            let newCenter = CGPoint(
                                x: centerPos.x + value.translation.width,
                                y: centerPos.y + value.translation.height
                            )
                            let newCorner = closestCorner(to: newCenter, in: geo.size)
                            withAnimation(.easeOut(duration: 0.2)) {
                                corner = newCorner
                                dragOffset = .zero
                            }
                        }
                )
            }
        }

        private var clipShape: some Shape {
            if #available(macOS 26.0, *) {
                return ConcentricRectangle(corners: .concentric(minimum: 8), isUniform: true)
            } else {
                return RoundedRectangle(cornerRadius: 8)
            }
        }

        enum Corner {
            case topLeft, topRight, bottomLeft, bottomRight

            var alignment: Alignment {
                switch self {
                case .topLeft: return .topLeading
                case .topRight: return .topTrailing
                case .bottomLeft: return .bottomLeading
                case .bottomRight: return .bottomTrailing
                }
            }
        }

        private func centerPosition(for corner: Corner, in containerSize: CGSize, barSize: CGSize) -> CGPoint {
            let halfWidth = barSize.width / 2 + padding
            let halfHeight = barSize.height / 2 + padding

            switch corner {
            case .topLeft:
                return CGPoint(x: halfWidth, y: halfHeight)
            case .topRight:
                return CGPoint(x: containerSize.width - halfWidth, y: halfHeight)
            case .bottomLeft:
                return CGPoint(x: halfWidth, y: containerSize.height - halfHeight)
            case .bottomRight:
                return CGPoint(x: containerSize.width - halfWidth, y: containerSize.height - halfHeight)
            }
        }

        private func closestCorner(to point: CGPoint, in containerSize: CGSize) -> Corner {
            let midX = containerSize.width / 2
            let midY = containerSize.height / 2

            if point.x < midX {
                return point.y < midY ? .topLeft : .bottomLeft
            } else {
                return point.y < midY ? .topRight : .bottomRight
            }
        }

        struct SearchButtonStyle: ButtonStyle {
            @State private var isHovered = false

            func makeBody(configuration: Configuration) -> some View {
                configuration.label
                    .foregroundStyle(isHovered || configuration.isPressed ? .primary : .secondary)
                    .padding(.horizontal, 2)
                    .frame(height: 26)
                    .background(
                        RoundedRectangle(cornerRadius: 6)
                            .fill(backgroundColor(isPressed: configuration.isPressed))
                    )
                    .onHover { hovering in
                        isHovered = hovering
                    }
                    .backport.pointerStyle(.link)
            }

            private func backgroundColor(isPressed: Bool) -> Color {
                if isPressed {
                    return Color.primary.opacity(0.2)
                } else if isHovered {
                    return Color.primary.opacity(0.1)
                } else {
                    return Color.clear
                }
            }
        }
    }

    /// A surface is terminology in Ghostty for a terminal surface, or a place where a terminal is actually drawn
    /// and interacted with. The word "surface" is used because a surface may represent a window, a tab,
    /// a split, a small preview pane, etc. It is ANYTHING that has a terminal drawn to it.
    struct SurfaceRepresentable: NSViewRepresentable {
        /// The view to render for the terminal surface.
        let view: SurfaceView

        /// The size of the frame containing this view. We use this to update the the underlying
        /// surface. This does not actually SET the size of our frame, this only sets the size
        /// of our Metal surface for drawing.
        ///
        /// Note: we do NOT use the NSView.resize function because SwiftUI on macOS 12
        /// does not call this callback (macOS 13+ does).
        ///
        /// The best approach is to wrap this view in a GeometryReader and pass in the geo.size.
        let size: CGSize

        func makeNSView(context: Context) -> SurfaceScrollView {
            return SurfaceScrollView(contentSize: size, surfaceView: view)
        }

        func updateNSView(_ scrollView: SurfaceScrollView, context: Context) {
            // SwiftUI may defer frame updates under system load (e.g., memory
            // pressure, heavy I/O) or when external window managers trigger rapid
            // layout changes. When that happens, the scroll view's bounds can
            // fall out of sync with the size reported by GeometryReader, causing
            // the surface to render at stale dimensions.
            guard scrollView.bounds.size != size else { return }
            scrollView.needsLayout = true
        }
    }

    /// The configuration for a surface. For any configuration not set, defaults will be chosen from
    /// libghostty, usually from the Ghostty configuration.
    struct SurfaceConfiguration {
        /// Explicit font size to use in points
        var fontSize: Float32?

        /// Explicit working directory. This is normalized on assignment to
        /// remove any redundant and trailing path separators.
        var workingDirectory: String? {
            get { normalizedWorkingDirectory }
            set { normalizedWorkingDirectory = newValue.map { FilePath($0).string } }
        }
        private var normalizedWorkingDirectory: String?

        /// Explicit command to set
        var command: String?

        /// Environment variables to set for the terminal
        var environmentVariables: [String: String] = [:]

        /// Extra input to send as stdin
        var initialInput: String?

        /// Wait after the command
        var waitAfterCommand: Bool = false

        /// Context for surface creation
        var context: ghostty_surface_context_e = GHOSTTY_SURFACE_CONTEXT_WINDOW

        init() {}

        init(from config: ghostty_surface_config_s) {
            self.fontSize = config.font_size
            if let workingDirectory = config.working_directory {
                self.workingDirectory = String.init(cString: workingDirectory, encoding: .utf8)
            }
            if let command = config.command {
                self.command = String.init(cString: command, encoding: .utf8)
            }

            // Convert the C env vars to Swift dictionary
            if config.env_var_count > 0, let envVars = config.env_vars {
                for i in 0..<config.env_var_count {
                    let envVar = envVars[i]
                    if let key = String(cString: envVar.key, encoding: .utf8),
                       let value = String(cString: envVar.value, encoding: .utf8) {
                        self.environmentVariables[key] = value
                    }
                }
            }
            self.context = config.context
        }

        /// Provides a C-compatible ghostty configuration within a closure. The configuration
        /// and all its string pointers are only valid within the closure.
        func withCValue<T>(view: SurfaceView, _ body: (inout ghostty_surface_config_s) throws -> T) rethrows -> T {
            var config = ghostty_surface_config_new()
            config.userdata = Unmanaged.passUnretained(view).toOpaque()
            config.platform_tag = GHOSTTY_PLATFORM_MACOS
            config.platform = ghostty_platform_u(macos: ghostty_platform_macos_s(
                nsview: Unmanaged.passUnretained(view).toOpaque()
            ))
            config.scale_factor = NSScreen.main!.backingScaleFactor

            // Zero is our default value that means to inherit the font size.
            config.font_size = fontSize ?? 0

            // Set wait after command
            config.wait_after_command = waitAfterCommand

            // Set context
            config.context = context

            // Use withCString to ensure strings remain valid for the duration of the closure
            return try workingDirectory.withCString { cWorkingDir in
                config.working_directory = cWorkingDir

                return try command.withCString { cCommand in
                    config.command = cCommand

                    return try initialInput.withCString { cInput in
                        config.initial_input = cInput

                        // Convert dictionary to arrays for easier processing
                        let keys = Array(environmentVariables.keys)
                        let values = Array(environmentVariables.values)

                        // Create C strings for all keys and values
                        return try keys.withCStrings { keyCStrings in
                            return try values.withCStrings { valueCStrings in
                                // Create array of ghostty_env_var_s
                                var envVars = [ghostty_env_var_s]()
                                envVars.reserveCapacity(environmentVariables.count)
                                for i in 0..<environmentVariables.count {
                                    envVars.append(ghostty_env_var_s(
                                        key: keyCStrings[i],
                                        value: valueCStrings[i]
                                    ))
                                }

                                return try envVars.withUnsafeMutableBufferPointer { buffer in
                                    config.env_vars = buffer.baseAddress
                                    config.env_var_count = environmentVariables.count
                                    return try body(&config)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Floating indicator that shows active key tables and pending key sequences.
    /// Displayed as a compact draggable pill that can be positioned at the top or bottom.
    struct KeyStateIndicator: View {
        let keyTables: [String]
        let keySequence: [KeyboardShortcut]

        @State private var isShowingPopover = false
        @State private var position: Position = .bottom
        @State private var dragOffset: CGSize = .zero
        @State private var isDragging = false

        private let padding: CGFloat = 8

        enum Position {
            case top, bottom

            var alignment: Alignment {
                switch self {
                case .top: return .top
                case .bottom: return .bottom
                }
            }

            var popoverEdge: Edge {
                switch self {
                case .top: return .top
                case .bottom: return .bottom
                }
            }

            var transitionEdge: Edge {
                popoverEdge
            }
        }

        var body: some View {
            Group {
                if !keyTables.isEmpty || !keySequence.isEmpty {
                    content
                        .backport.pointerStyle(!keyTables.isEmpty ? .link : nil)
                }
            }
            .transition(.move(edge: position.transitionEdge).combined(with: .opacity))
            .animation(.spring(response: 0.3, dampingFraction: 0.8), value: keyTables)
            .animation(.spring(response: 0.3, dampingFraction: 0.8), value: keySequence.count)
        }

        var content: some View {
            indicatorContent
                .offset(dragOffset)
                .padding(padding)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: position.alignment)
                .highPriorityGesture(
                    DragGesture(coordinateSpace: .local)
                        .onChanged { value in
                            isDragging = true
                            dragOffset = CGSize(width: 0, height: value.translation.height)
                        }
                        .onEnded { value in
                            isDragging = false
                            let dragThreshold: CGFloat = 50

                            withAnimation(.easeOut(duration: 0.2)) {
                                if position == .bottom && value.translation.height < -dragThreshold {
                                    position = .top
                                } else if position == .top && value.translation.height > dragThreshold {
                                    position = .bottom
                                }
                                dragOffset = .zero
                            }
                        }
                )
        }

        @ViewBuilder
        private var indicatorContent: some View {
            HStack(alignment: .center, spacing: 8) {
                // Key table indicator
                if !keyTables.isEmpty {
                    HStack(spacing: 5) {
                        Image(systemName: "keyboard.badge.ellipsis")
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)

                        // Show table stack with arrows between them
                        ForEach(Array(keyTables.enumerated()), id: \.offset) { index, table in
                            if index > 0 {
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(.tertiary)
                            }
                            Text(verbatim: table)
                                .font(.system(size: 13, weight: .medium, design: .rounded))
                        }
                    }
                }

                // Separator when both are active
                if !keyTables.isEmpty && !keySequence.isEmpty {
                    Divider()
                        .frame(height: 14)
                }

                // Key sequence indicator
                if !keySequence.isEmpty {
                    HStack(alignment: .center, spacing: 4) {
                        ForEach(Array(keySequence.enumerated()), id: \.offset) { _, key in
                            KeyCap(key.description)
                        }

                        // Animated ellipsis to indicate waiting for next key
                        PendingIndicator(paused: isDragging)
                    }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background {
                Capsule()
                    .fill(.regularMaterial)
                    .overlay {
                        Capsule()
                            .strokeBorder(Color.primary.opacity(0.15), lineWidth: 1)
                    }
                    .shadow(color: .black.opacity(0.2), radius: 8, y: 2)
            }
            .contentShape(Capsule())
            .backport.pointerStyle(.link)
            .popover(isPresented: $isShowingPopover, arrowEdge: position.popoverEdge) {
                VStack(alignment: .leading, spacing: 8) {
                    if !keyTables.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Label("Key Table", systemImage: "keyboard.badge.ellipsis")
                                .font(.headline)
                            Text("A key table is a named set of keybindings, activated by some other key. Keys are interpreted using this table until it is deactivated.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }

                    if !keyTables.isEmpty && !keySequence.isEmpty {
                        Divider()
                    }

                    if !keySequence.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Label("Key Sequence", systemImage: "character.cursor.ibeam")
                                .font(.headline)
                            Text("A key sequence is a series of key presses that trigger an action. A pending key sequence is currently active.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .padding()
                .frame(maxWidth: 400)
                .fixedSize(horizontal: false, vertical: true)
            }
            .onTapGesture {
                isShowingPopover.toggle()
            }
        }

        /// A small keycap-style view for displaying keyboard shortcuts
        struct KeyCap: View {
            let text: String

            init(_ text: String) {
                self.text = text
            }

            var body: some View {
                Text(verbatim: text)
                    .font(.system(size: 12, weight: .medium, design: .rounded))
                    .padding(.horizontal, 5)
                    .padding(.vertical, 2)
                    .background(
                        RoundedRectangle(cornerRadius: 4)
                            .fill(Color(NSColor.controlBackgroundColor))
                            .shadow(color: .black.opacity(0.12), radius: 0.5, y: 0.5)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 4)
                            .strokeBorder(Color.primary.opacity(0.15), lineWidth: 0.5)
                    )
            }
        }

        /// Animated dots to indicate waiting for the next key
        struct PendingIndicator: View {
            @State private var animationPhase: Double = 0
            let paused: Bool

            var body: some View {
                TimelineView(.animation(paused: paused)) { context in
                    HStack(spacing: 2) {
                        ForEach(0..<3, id: \.self) { index in
                            Circle()
                                .fill(Color.secondary)
                                .frame(width: 4, height: 4)
                                .opacity(dotOpacity(for: index))
                        }
                    }
                    .onChange(of: context.date.timeIntervalSinceReferenceDate) { newValue in
                        animationPhase = newValue
                    }
                }
            }

            private func dotOpacity(for index: Int) -> Double {
                let phase = animationPhase
                let offset = Double(index) / 3.0
                let wave = sin((phase + offset) * .pi * 2)
                return 0.3 + 0.7 * ((wave + 1) / 2)
            }
        }
    }

    /// Visual overlay that shows a border around the edges when the bell rings with border feature enabled.
    struct BellBorderOverlay: View {
        let bell: Bool

        var body: some View {
            Rectangle()
                .strokeBorder(
                    Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.5),
                    lineWidth: 3
                )
                .allowsHitTesting(false)
                .opacity(bell ? 1.0 : 0.0)
                .animation(.easeInOut(duration: 0.3), value: bell)
        }
    }

    /// Visual overlay that briefly highlights a surface to draw attention to it.
    /// Uses a soft, soothing highlight with a pulsing border effect.
    struct HighlightOverlay: View {
        @Environment(\.ghosttyAccentColor) private var accentColor

        let highlighted: Bool

        @State private var borderPulse: Bool = false

        var body: some View {
            ZStack {
                Rectangle()
                    .fill(
                        RadialGradient(
                            gradient: Gradient(colors: [
                                accentColor.opacity(0.12),
                                accentColor.opacity(0.03),
                                Color.clear
                            ]),
                            center: .center,
                            startRadius: 0,
                            endRadius: 2000
                        )
                    )

                Rectangle()
                    .strokeBorder(
                        LinearGradient(
                            gradient: Gradient(colors: [
                                accentColor.opacity(0.8),
                                accentColor.opacity(0.5),
                                accentColor.opacity(0.8)
                            ]),
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        lineWidth: borderPulse ? 4 : 2
                    )
                    .shadow(color: accentColor.opacity(borderPulse ? 0.8 : 0.6), radius: borderPulse ? 12 : 8, x: 0, y: 0)
                    .shadow(color: accentColor.opacity(borderPulse ? 0.5 : 0.3), radius: borderPulse ? 24 : 16, x: 0, y: 0)
            }
            .allowsHitTesting(false)
            .opacity(highlighted ? 1.0 : 0.0)
            .animation(.easeOut(duration: 0.4), value: highlighted)
            .onChange(of: highlighted) { newValue in
                if newValue {
                    withAnimation(.easeInOut(duration: 0.4).repeatForever(autoreverses: true)) {
                        borderPulse = true
                    }
                } else {
                    withAnimation(.easeOut(duration: 0.4)) {
                        borderPulse = false
                    }
                }
            }
        }
    }

    /// A cheap, finite highlight that remains stable when the split tree is rebuilt.
    struct FiniteHighlightOverlay: View {
        @Environment(\.ghosttyAccentColor) private var accentColor

        let highlight: Ghostty.OSSurfaceView.FiniteHighlight?

        var body: some View {
            Rectangle()
                .strokeBorder(accentColor.opacity(0.8), lineWidth: highlight?.lineWidth ?? 0)
                .allowsHitTesting(false)
                .opacity(highlight == nil ? 0 : 1)
                .animation(highlight == nil ? .easeOut(duration: 0.2) : nil, value: highlight)
        }
    }

    struct NavigationWarningOverlay: View {
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.ghosttyAccentColor) private var accentColor

        let warning: OSSurfaceView.NavigationWarning?
        let scope: OSSurfaceView.NavigationWarning.Scope

        var body: some View {
            if let warning, warning.scope == scope {
                TimelineView(.animation(paused: reduceMotion)) { context in
                    Rectangle()
                        .strokeBorder(
                            accentColor.opacity(0.95),
                            lineWidth: OSSurfaceView.FiniteHighlight.zoom.lineWidth)
                        .allowsHitTesting(false)
                        .opacity(warning.isVisible(
                            at: context.date,
                            reduceMotion: reduceMotion) ? 1 : 0)
                }
            }
        }
    }

    struct NotificationAttentionOverlay: View {
        @Environment(\.ghosttyAccentColor) private var accentColor

        let active: Bool

        var body: some View {
            Rectangle()
                .strokeBorder(
                    accentColor.opacity(0.8),
                    lineWidth: Ghostty.OSSurfaceView.FiniteHighlight.focus.lineWidth)
                .allowsHitTesting(false)
                .opacity(active ? 0.65 : 0)
        }
    }

    // MARK: Surface Status Badges

    struct SurfaceStatusBadges: View {
        let readonly: Bool
        let workingDirectory: WorkingDirectoryBadge.Presentation?
        let workingDirectoryPosition: CGPoint?
        let largeWorkingDirectoryLabels: Bool
        let windowFocus: Bool
        let workingDirectoryActivity: Bool
        let workingDirectoryFlashOpacity: Double
        let onDisableReadonly: () -> Void

        private var workingDirectoryLayout: WorkingDirectoryBadge.Layout? {
            workingDirectory.map {
                WorkingDirectoryBadge.paneLayout(
                    $0,
                    largeInactiveLabels: largeWorkingDirectoryLabels)
            }
        }

        var body: some View {
            ZStack {
                VStack(alignment: .trailing, spacing: 6) {
                    if readonly {
                        ReadonlyBadge(onDisable: onDisableReadonly)
                    }

                    if workingDirectoryLayout == .compact,
                       workingDirectoryPosition == nil,
                       let workingDirectory {
                        WorkingDirectoryBadge(
                            presentation: workingDirectory,
                            isActive: workingDirectoryActivity,
                            flashOpacity: workingDirectoryFlashOpacity)
                            .transition(.opacity)
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)

                if workingDirectoryLayout == .large, let workingDirectory {
                    GeometryReader { geometry in
                        WorkingDirectoryBadge(
                            presentation: workingDirectory,
                            isActive: workingDirectoryActivity,
                            flashOpacity: workingDirectoryFlashOpacity,
                            layout: .large,
                            windowFocus: windowFocus)
                            .frame(maxWidth: geometry.size.width * 0.75)
                            .position(
                                x: geometry.size.width / 2,
                                y: geometry.size.height / 2)
                    }
                    .transition(.opacity)
                } else if let workingDirectoryPosition, let workingDirectory {
                    GeometryReader { _ in
                        WorkingDirectoryBadge(
                            presentation: workingDirectory,
                            isActive: workingDirectoryActivity,
                            flashOpacity: workingDirectoryFlashOpacity)
                            .position(
                                x: workingDirectoryPosition.x,
                                y: workingDirectoryPosition.y)
                    }
                    .transition(.opacity)
                }
            }
        }
    }

    struct WorkingDirectoryBadge: View {
        enum Style: Equatable {
            case normal
            case focused
        }

        enum Layout: Equatable {
            case compact
            case large
        }

        struct Presentation: Equatable {
            let name: String
            let style: Style
        }

        let presentation: Presentation
        let isActive: Bool
        let flashOpacity: Double
        let flashesWhenFocused: Bool
        let layout: Layout
        let windowFocus: Bool
        let customAccessibilityLabel: String?

        init(
            presentation: Presentation,
            isActive: Bool = false,
            flashOpacity: Double = 0,
            flashesWhenFocused: Bool = false,
            layout: Layout = .compact,
            windowFocus: Bool = true,
            accessibilityLabel: String? = nil
        ) {
            self.presentation = presentation
            self.isActive = isActive
            self.flashOpacity = flashOpacity
            self.flashesWhenFocused = flashesWhenFocused
            self.layout = layout
            self.windowFocus = windowFocus
            self.customAccessibilityLabel = accessibilityLabel
        }

        static func paneLayout(
            _ presentation: Presentation,
            largeInactiveLabels: Bool
        ) -> Layout {
            presentation.style == .normal && largeInactiveLabels ? .large : .compact
        }

        static func presentation(
            pwd: String?,
            isFocusedSurface: Bool,
            windowFocus: Bool,
            showFocusedSurface: Bool = false
        ) -> Presentation? {
            guard let name = name(pwd: pwd) else { return nil }
            if windowFocus && isFocusedSurface && !showFocusedSurface { return nil }

            return .init(
                name: name,
                style: isFocusedSurface ? .focused : .normal
            )
        }

        static func commonName(pwds: [String?]) -> String? {
            guard !pwds.isEmpty else { return nil }
            let names = pwds.compactMap { name(pwd: $0) }
            guard names.count == pwds.count, let first = names.first else { return nil }
            return names.dropFirst().allSatisfy { $0 == first } ? first : nil
        }

        static func quadrantPresentation(
            name: String?,
            isFocusedQuadrant: Bool,
            windowFocus: Bool,
            showFocused: Bool = false
        ) -> Presentation? {
            guard let name else { return nil }
            if isFocusedQuadrant && windowFocus && !showFocused { return nil }

            return .init(
                name: name,
                style: isFocusedQuadrant ? .focused : .normal
            )
        }

        private static func name(pwd: String?) -> String? {
            guard let pwd, !pwd.isEmpty else { return nil }
            let path = FilePath(pwd)
            return path.lastComponent?.string ?? path.string
        }

        static func width(for presentation: Presentation) -> CGFloat {
            let weight: NSFont.Weight = presentation.style == .focused ? .semibold : .medium
            let font = NSFont.systemFont(ofSize: 12, weight: weight)
            return ceil((presentation.name as NSString).size(withAttributes: [.font: font]).width) + 16
        }

        static func showsActivityIndicator(
            isActive: Bool,
            presentation: Presentation
        ) -> Bool {
            isActive && presentation.style == .normal
        }

        static func showsActivityFlash(
            stopped: Bool,
            presentation: Presentation,
            includeFocused: Bool = false
        ) -> Bool {
            stopped && (includeFocused || presentation.style == .normal)
        }

        static func cursorMoved(
            from initialPosition: CGPoint,
            currentPosition: () -> CGPoint?
        ) async throws -> Bool {
            let clock = ContinuousClock()
            let deadline = clock.now.advanced(by: .seconds(2))
            while clock.now < deadline {
                try await clock.sleep(for: .milliseconds(16))
                if currentPosition() != initialPosition { return true }
            }
            return false
        }

        static func cursorAdjacentX(
            containerWidth: CGFloat,
            cursorCenterX: CGFloat,
            labelWidth: CGFloat,
            cellWidth: CGFloat
        ) -> CGFloat? {
            let offset = cellWidth * 5 + (cellWidth + labelWidth) / 2
            let right = cursorCenterX + offset
            if right + labelWidth / 2 <= containerWidth - 8 { return right }

            let left = cursorCenterX - offset
            return left - labelWidth / 2 >= 8 ? left : nil
        }

        var body: some View {
            let focused = presentation.style == .focused
            let large = layout == .large
            let showsActivity = Self.showsActivityIndicator(
                isActive: isActive,
                presentation: presentation)
            let accessibilityLabel = focused
                ? "Focused terminal working directory: \(presentation.name)"
                : "Terminal working directory: \(presentation.name)" +
                    (showsActivity ? ", activity in progress" : "")

            Text(presentation.name)
                .lineLimit(1)
                .truncationMode(.middle)
                .underline(showsActivity)
                .font(large
                    ? .title.weight(.semibold)
                    : .system(size: 12, weight: focused ? .semibold : .medium))
                .padding(.horizontal, large ? 16 : 8)
                .padding(.vertical, large ? 10 : 4)
                .background(background(focused: focused, cornerRadius: large ? 10 : 6))
                .foregroundStyle(focused ? Color.white : Color.primary)
                .opacity(large && windowFocus ? 0.8 : 1)
                .allowsHitTesting(false)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(customAccessibilityLabel ?? accessibilityLabel)
        }

        private func background(focused: Bool, cornerRadius: CGFloat) -> some View {
            ZStack {
                RoundedRectangle(cornerRadius: cornerRadius)
                    .fill(focused ? AnyShapeStyle(Color.black) : AnyShapeStyle(.regularMaterial))
                    .overlay(
                        RoundedRectangle(cornerRadius: cornerRadius)
                            .strokeBorder(
                                focused ? Color.accentColor : Color.secondary.opacity(0.35),
                                lineWidth: 1
                            )
                    )

                RoundedRectangle(cornerRadius: cornerRadius)
                    .fill(Color.accentColor)
                    .opacity(focused && !flashesWhenFocused ? 0 : flashOpacity)
            }
        }
    }

    struct QuadrantWorkingDirectoryBadge: View {
        let presentation: WorkingDirectoryBadge.Presentation
        let isActive: Bool
        let flashOpacity: Double
        let windowFocus: Bool

        var body: some View {
            let focused = presentation.style == .focused
            let showsActivity = WorkingDirectoryBadge.showsActivityIndicator(
                isActive: isActive,
                presentation: presentation)
            let accessibilityLabel = (focused
                ? "Focused quadrant working directory: \(presentation.name)"
                : "Quadrant working directory: \(presentation.name)") +
                    (showsActivity ? ", activity in progress" : "")

            WorkingDirectoryBadge(
                presentation: presentation,
                isActive: isActive,
                flashOpacity: flashOpacity,
                flashesWhenFocused: true,
                layout: .large,
                windowFocus: windowFocus,
                accessibilityLabel: accessibilityLabel)
        }
    }

    // MARK: Readonly Badge

    /// A badge overlay that indicates a surface is in readonly mode.
    /// Positioned in the top-right corner and styled to be noticeable but unobtrusive.
    struct ReadonlyBadge: View {
        let onDisable: () -> Void

        @State private var showingPopover = false

        private let badgeColor = Color(hue: 0.08, saturation: 0.5, brightness: 0.8)

        var body: some View {
            HStack(spacing: 5) {
                Image(systemName: "eye.fill")
                    .font(.system(size: 12))
                Text("Read-only")
                    .font(.system(size: 12, weight: .medium))
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(badgeBackground)
            .foregroundStyle(badgeColor)
            .onTapGesture {
                showingPopover = true
            }
            .backport.pointerStyle(.link)
            .popover(isPresented: $showingPopover, arrowEdge: .bottom) {
                ReadonlyPopoverView(onDisable: onDisable, isPresented: $showingPopover)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Read-only terminal")
        }

        private var badgeBackground: some View {
            RoundedRectangle(cornerRadius: 6)
                .fill(.regularMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .strokeBorder(Color.orange.opacity(0.6), lineWidth: 1.5)
                )
        }
    }

    struct ReadonlyPopoverView: View {
        let onDisable: () -> Void
        @Binding var isPresented: Bool

        var body: some View {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        Image(systemName: "eye.fill")
                            .foregroundColor(.orange)
                            .font(.system(size: 13))
                        Text("Read-Only Mode")
                            .font(.system(size: 13, weight: .semibold))
                    }

                    Text("This terminal is in read-only mode. You can still view, select, and scroll through the content, but no input events will be sent to the running application.")
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack {
                    Spacer()

                    Button("Disable") {
                        onDisable()
                        isPresented = false
                    }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
            .padding(16)
            .frame(width: 280)
        }
    }

    /// When changing the split state, or going full screen (native or non), the terminal view
    /// will lose focus. There has to be some nice SwiftUI-native way to fix this but I can't
    /// figure it out so we're going to do this hacky thing to bring focus back to the terminal
    /// that should have it.
    static func moveFocus(
        to: SurfaceView,
        from: SurfaceView? = nil,
        delay: TimeInterval? = nil
    ) {
        // The whole delay machinery is a bit of a hack to work around a
        // situation where the window is destroyed and the surface view
        // will never be attached to a window. Realistically, we should
        // handle this upstream but we also don't want this function to be
        // a source of infinite loops.

        // Our max delay before we give up
        let maxDelay: TimeInterval = 0.5
        guard (delay ?? 0) < maxDelay else { return }

        // We start at a 50 millisecond delay and do a doubling backoff
        let nextDelay: TimeInterval = if let delay {
            delay * 2
        } else {
            // 100 milliseconds
            0.05
        }

        let work: DispatchWorkItem = .init {
            // If the callback runs before the surface is attached to a view
            // then the window will be nil. We just reschedule in that case.
            guard let window = to.window else {
                moveFocus(to: to, from: from, delay: nextDelay)
                return
            }

            // If we had a previously focused node and its not where we're sending
            // focus, make sure that we explicitly tell it to lose focus. In theory
            // we should NOT have to do this but the focus callback isn't getting
            // called for some reason.
            if let from = from {
                _ = from.resignFirstResponder()
            }

            window.makeFirstResponder(to)
        }

        let queue = DispatchQueue.main
        if let delay {
            queue.asyncAfter(deadline: .now() + delay, execute: work)
        } else {
            queue.async(execute: work)
        }
    }
}

// MARK: Surface Environment Keys

private struct GhosttySurfaceViewKey: EnvironmentKey {
    static let defaultValue: Ghostty.SurfaceView? = nil
}

private struct GhosttyLastFocusedSurfaceKey: EnvironmentKey {
    /// Optional read-only last-focused surface reference. If a surface view is currently focused this
    /// is equal to the currently focused surface.
    static let defaultValue: Weak<Ghostty.SurfaceView>? = nil
}

private struct GhosttyAccentColorKey: EnvironmentKey {
    static let defaultValue = Color.accentColor
}

private struct GhosttyWorkingDirectoryLabelsHiddenKey: EnvironmentKey {
    static let defaultValue = false
}

private struct GhosttyWorkingDirectoryLabelsLargeKey: EnvironmentKey {
    static let defaultValue = false
}

extension Ghostty {
    struct SurfaceWindowFocusKey: PreferenceKey {
        static let defaultValue = true

        static func reduce(value: inout Bool, nextValue: () -> Bool) {
            value = value && nextValue()
        }
    }
}

extension EnvironmentValues {
    var ghosttyAccentColor: Color {
        get { self[GhosttyAccentColorKey.self] }
        set { self[GhosttyAccentColorKey.self] = newValue }
    }

    var ghosttySurfaceView: Ghostty.SurfaceView? {
        get { self[GhosttySurfaceViewKey.self] }
        set { self[GhosttySurfaceViewKey.self] = newValue }
    }

    var ghosttyLastFocusedSurface: Weak<Ghostty.SurfaceView>? {
        get { self[GhosttyLastFocusedSurfaceKey.self] }
        set { self[GhosttyLastFocusedSurfaceKey.self] = newValue }
    }

    var ghosttyWorkingDirectoryLabelsHidden: Bool {
        get { self[GhosttyWorkingDirectoryLabelsHiddenKey.self] }
        set { self[GhosttyWorkingDirectoryLabelsHiddenKey.self] = newValue }
    }

    var ghosttyWorkingDirectoryLabelsLarge: Bool {
        get { self[GhosttyWorkingDirectoryLabelsLargeKey.self] }
        set { self[GhosttyWorkingDirectoryLabelsLargeKey.self] = newValue }
    }
}

extension View {
    func ghosttyAccentColor(_ color: Color) -> some View {
        environment(\.ghosttyAccentColor, color)
            .accentColor(color)
    }

    func ghosttySurfaceView(_ surfaceView: Ghostty.SurfaceView?) -> some View {
        environment(\.ghosttySurfaceView, surfaceView)
    }

    /// The most recently focused surface (can be currently focused if the surface is currently focused).
    func ghosttyLastFocusedSurface(_ surfaceView: Weak<Ghostty.SurfaceView>?) -> some View {
        environment(\.ghosttyLastFocusedSurface, surfaceView)
    }
}

// MARK: Surface Focus Keys

extension FocusedValues {
    var ghosttySurfaceView: Ghostty.SurfaceView? {
        get { self[FocusedGhosttySurface.self] }
        set { self[FocusedGhosttySurface.self] = newValue }
    }

    struct FocusedGhosttySurface: FocusedValueKey {
        typealias Value = Ghostty.SurfaceView
    }

    var ghosttySurfacePwd: String? {
        get { self[FocusedGhosttySurfacePwd.self] }
        set { self[FocusedGhosttySurfacePwd.self] = newValue }
    }

    struct FocusedGhosttySurfacePwd: FocusedValueKey {
        typealias Value = String
    }

    var ghosttySurfaceCellSize: CGSize? {
        get { self[FocusedGhosttySurfaceCellSize.self] }
        set { self[FocusedGhosttySurfaceCellSize.self] = newValue }
    }

    struct FocusedGhosttySurfaceCellSize: FocusedValueKey {
        typealias Value = CGSize
    }
}
