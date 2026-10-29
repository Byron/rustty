//! Shared tab, pane, and quadrant presentation rules from Ghostty Local.
use crate::workspace::{Id, Tab};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PROGRESS_TIMEOUT: Duration = Duration::from_secs(15);
pub const FLASH_DURATION: Duration = Duration::from_millis(1200);

pub fn zoom_label(tab: &Tab) -> Option<&'static str> {
    let zoom = tab.zoom?;
    Some(match tab.quadrant_zoom {
        None => "Zoom",
        Some(quadrant) if quadrant == zoom => "Quad-zoom",
        Some(_) => "Zoom within quad-zoom",
    })
}

/// Draw tab zoom layers in square bounds: an outline for quad, a fill for pane.
pub fn paint_zoom_icon(painter: &egui::Painter, rect: egui::Rect, tab: &Tab, color: egui::Color32) {
    if tab.zoom.is_none() {
        return;
    }
    let quad = tab.quadrant_zoom.is_some();
    if quad {
        painter.rect_stroke(
            rect,
            0,
            egui::Stroke::new(1.5, color),
            egui::StrokeKind::Inside,
        );
    }
    if tab.zoom != tab.quadrant_zoom {
        painter.rect_filled(
            if quad {
                rect.shrink(rect.width() * 0.25)
            } else {
                rect
            },
            0,
            color,
        );
    }
}

/// Typing restarts the visible phase; idle redraws happen only at its boundaries.
pub fn cursor_blink_phase(started: Instant, now: Instant) -> (bool, Instant) {
    let interval = Duration::from_millis(600).as_nanos();
    let elapsed = now.saturating_duration_since(started).as_nanos();
    (
        (elapsed / interval).is_multiple_of(2),
        now + Duration::from_nanos((interval - elapsed % interval) as u64),
    )
}

/// A focus change shows its directory once, until typing, another click, or expiry.
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusHint {
    pane: Option<Id>,
    expires: Option<Instant>,
}
impl FocusHint {
    pub fn focus(&mut self, pane: Option<Id>, now: Instant) -> bool {
        if self.pane == pane {
            return false;
        }
        self.pane = pane;
        self.expires = pane.map(|_| now + Duration::from_secs(2));
        true
    }

    pub fn dismiss(&mut self) {
        self.expires = None;
    }

    pub fn visible(&self, pane: Id, now: Instant) -> bool {
        self.pane == Some(pane) && self.deadline(now).is_some()
    }

    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        self.expires.filter(|&expires| now < expires)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TitleActivity {
    #[default]
    Idle,
    Working,
    Waiting,
}
impl TitleActivity {
    fn parse(title: &str) -> Self {
        if ["[ ! ] Action Required", "[ . ] Action Required"]
            .iter()
            .any(|prefix| {
                title
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.is_empty() || s.starts_with(" | "))
            })
        {
            return Self::Waiting;
        }
        // Match Ghostty Local's leading agent spinner, excluding filenames and
        // incidental spinner characters later in a title.
        let mut chars = title.chars();
        if chars.next().is_some_and(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c))
            && chars.next().is_none_or(char::is_whitespace)
        {
            Self::Working
        } else {
            Self::Idle
        }
    }
}

/// Runtime state for one pane. Consume title, command, and progress effects in
/// their original order; a command boundary clears the previous work report.
#[derive(Clone, Copy, Debug, Default)]
pub struct Activity {
    command_running: bool,
    title: TitleActivity,
    reported_progress: bool,
    progress: Option<Progress>,
    progress_deadline: Option<Instant>,
    progress_started: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub state: u8,
    pub value: Option<u8>,
}
impl Progress {
    /// Ghostty shows an unspecified paused report as a full, stationary bar.
    pub fn percentage(self) -> Option<u8> {
        self.value.or((self.state == 4).then_some(100))
    }
}

impl Activity {
    /// Underline inactive tabs/labels while a command or any progress report exists.
    pub fn is_active(&self) -> bool {
        self.command_running || self.progress_deadline.is_some()
    }

    /// Count explicitly working panes, excluding programs waiting for input.
    /// Paused/error progress and a running shell command alone do not count.
    pub fn reported_active(&self) -> bool {
        self.title != TitleActivity::Waiting
            && (self.title == TitleActivity::Working || self.reported_progress)
    }

    pub fn title_changed(&mut self, title: &str) {
        self.title = TitleActivity::parse(title);
    }

    /// Returns true only when this pane changes from active to idle.
    pub fn command_started(&mut self) -> bool {
        self.command(true)
    }

    /// A remaining progress report keeps the pane active until removed or expired.
    pub fn command_finished(&mut self) -> bool {
        self.command(false)
    }

    fn command(&mut self, running: bool) -> bool {
        let was_active = self.is_active();
        self.command_running = running;
        self.title = TitleActivity::Idle;
        self.reported_progress = false;
        was_active && !self.is_active()
    }

    /// OSC 9;4 states: 0 removes, 1 sets, 2 errors, 3 is indeterminate, 4 pauses.
    /// Returns a per-pane stop transition, even if another pane is still active.
    pub fn progress_reported(&mut self, state: u8, value: Option<u8>, now: Instant) -> bool {
        if state > 4 {
            return false;
        }
        let was_active = self.is_active();
        self.progress = (state != 0).then_some(Progress {
            state,
            value: value.map(|value| value.min(100)),
        });
        if self
            .progress
            .is_none_or(|progress| progress.percentage().is_some())
        {
            self.reset_progress_animation();
        }
        self.progress_deadline = (state != 0).then(|| now + PROGRESS_TIMEOUT);
        self.reported_progress = matches!(state, 1 | 3);
        was_active && !self.is_active()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.progress_deadline
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }

    pub fn reset_progress_animation(&mut self) {
        self.progress_started = None;
    }

    pub fn progress_started(&self) -> Option<Instant> {
        self.progress_started
    }

    /// Ghostty's bouncing view starts on appearance and retains its phase when
    /// another indeterminate report arrives. The result is a fraction of pane width.
    pub fn progress_offset(&mut self, now: Instant) -> f32 {
        if self
            .progress
            .is_none_or(|progress| progress.percentage().is_some())
        {
            return 0.0;
        }
        let started = *self.progress_started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started).as_secs_f64();
        let leg = (elapsed % 2.4) / 1.2;
        let x = if leg <= 1.0 { leg } else { 2.0 - leg };
        // SwiftUI easeInOut is cubic-bezier(.42, 0, .58, 1). Solve its x
        // coordinate before evaluating y; the derivative never falls below .87.
        let mut t = x;
        for _ in 0..5 {
            t -= (((0.52 * t - 0.78) * t + 1.26) * t - x) / ((1.56 * t - 1.56) * t + 1.26);
        }
        (0.75 * (3.0 - 2.0 * t) * t * t) as f32
    }

    /// Call at the deadline, including for hidden panes, to expire stale progress.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self
            .progress_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.progress_reported(0, None, now)
        } else {
            false
        }
    }

    /// Clear transient reports when the pane process exits.
    pub fn clear(&mut self) -> bool {
        let was_active = self.is_active();
        *self = Self::default();
        was_active
    }
}

/// A finite completion pulse: 200 ms fade-in followed by a 1 s fade-out.
/// Scale opacity by 0.45 for tabs; directory badges use the full opacity.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompletionFlash {
    started: Option<Instant>,
}
impl CompletionFlash {
    /// Inactive tabs restart their flash when another pane completes.
    pub fn start(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// Pane and quadrant labels let the current pulse finish without restarting.
    pub fn start_if_idle(&mut self, now: Instant) {
        if self.next_repaint(now).is_none() {
            self.start(now);
        }
    }

    pub fn clear(&mut self) {
        self.started = None;
    }

    pub fn opacity(&self, now: Instant) -> f32 {
        let Some(started) = self.started else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(started).as_secs_f32();
        if elapsed < 0.2 {
            let t = elapsed / 0.2;
            1.0 - (1.0 - t).powi(3)
        } else if elapsed < 1.2 {
            let t = elapsed - 0.2;
            1.0 - t * t * (3.0 - 2.0 * t)
        } else {
            0.0
        }
    }

    /// No deadline remains after the pulse, so idle windows can sleep again.
    pub fn next_repaint(&self, now: Instant) -> Option<Instant> {
        let end = self.started? + FLASH_DURATION;
        (now < end).then(|| (now + Duration::from_nanos(16_666_667)).min(end))
    }
}

pub fn directory_name(path: &Path) -> Option<String> {
    let name = path.file_name().unwrap_or(path.as_os_str());
    (!name.is_empty()).then(|| name.to_string_lossy().into_owned())
}

/// Add parent components only when distinct directories have the same basename.
pub fn directory_labels<'a>(
    directories: impl IntoIterator<Item = (Id, &'a Path)>,
) -> HashMap<Id, String> {
    let mut groups = HashMap::<String, HashMap<&Path, Vec<Id>>>::new();
    for (id, path) in directories {
        if let Some(name) = directory_name(path) {
            groups
                .entry(name)
                .or_default()
                .entry(path)
                .or_default()
                .push(id);
        }
    }
    let mut labels = HashMap::new();
    for (name, paths) in groups {
        for (&path, ids) in &paths {
            let label = if paths.len() == 1 {
                name.clone()
            } else {
                let components: Vec<_> = path.components().collect();
                let suffix = (0..components.len())
                    .rev()
                    .map(|start| components[start..].iter().collect::<PathBuf>())
                    .find(|suffix| {
                        paths
                            .keys()
                            .all(|other| *other == path || !other.ends_with(suffix))
                    })
                    .unwrap_or_else(|| components.iter().collect());
                suffix.to_string_lossy().into_owned()
            };
            labels.extend(ids.iter().map(|&id| (id, label.clone())));
        }
    }
    labels
}

/// A missing label prevents a shared badge rather than hiding that pane.
pub fn common_label<'a>(labels: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let mut labels = labels.into_iter();
    let first = labels.next()??;
    (!first.is_empty() && labels.all(|label| label == Some(first))).then(|| first.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryLabel {
    pub name: String,
    pub selected: bool,
    pub focused: bool,
}
impl DirectoryLabel {
    /// A selected label is normally hidden only while its window is focused.
    /// `show_focused` reveals it during quadrant peek or the temporary focus hint.
    pub fn new(
        name: Option<String>,
        selected: bool,
        window_focused: bool,
        show_focused: bool,
    ) -> Option<Self> {
        let name = name.filter(|name| !name.is_empty())?;
        (!(selected && window_focused && !show_focused)).then_some(Self {
            name,
            selected,
            focused: selected && window_focused,
        })
    }

    pub fn large(&self, large_inactive: bool) -> bool {
        self.selected || large_inactive
    }

    /// A hidden focused aggregate leaves room for its focused pane's hint only.
    pub fn suppresses_panel_name(&self, panel_selected: bool, peek: bool) -> bool {
        !self.focused || peek || !panel_selected
    }

    pub fn centered_in_pane(&self, tab: &Tab, pane: Id, quadrant: Option<Id>) -> bool {
        self.large(
            quadrant.is_some_and(|id| tab.root.node(id).is_some_and(|node| node.contains(pane))),
        )
    }

    pub fn shows_activity(&self, active: bool) -> bool {
        active && !self.selected
    }

    /// Quadrant labels include the focused quadrant; individual pane labels do not.
    pub fn shows_flash(&self, stopped: bool, include_focused: bool) -> bool {
        stopped && (include_focused || !self.selected)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabAccent {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
}
impl TabAccent {
    pub fn for_window(self, focused: bool) -> Self {
        if focused {
            return self;
        }
        let [r, g, b] = self.background.map(u32::from);
        let gray = (299 * r + 587 * g + 114 * b + 500) / 1000;
        let background = self
            .background
            .map(|channel| ((95 * gray + 5 * u32::from(channel) + 50) / 100) as u8);
        Self::new(None, background)
    }

    pub fn frame_background(self, background: [u8; 3], mix: f32) -> [u8; 3] {
        std::array::from_fn(|i| {
            (f32::from(background[i]) * (1.0 - mix) + f32::from(self.background[i]) * mix).round()
                as u8
        })
    }

    /// Unassigned tabs inherit the platform accent supplied by the host.
    pub fn new(assigned: Option<[u8; 3]>, platform_accent: [u8; 3]) -> Self {
        let background = assigned.unwrap_or(platform_accent);
        let [r, g, b] = background.map(u32::from);
        let light = 299 * r + 587 * g + 114 * b > 127_500;
        Self {
            background,
            foreground: if light { [0; 3] } else { [255; 3] },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_labels_distinguish_pane_quadrant_and_nested_zoom() {
        let mut tab = Tab::new(1, 2, "/tmp".into());
        tab.split(3, 4, rustty::config::Direction::Right, "/tmp".into());
        tab.split(5, 6, rustty::config::Direction::Down, "/tmp".into());
        tab.split(7, 8, rustty::config::Direction::Right, "/tmp".into());
        assert_eq!(zoom_label(&tab), None);
        tab.toggle_zoom();
        assert_eq!(zoom_label(&tab), Some("Zoom"));
        tab.toggle_quadrant_zoom();
        assert_eq!(zoom_label(&tab), Some("Quad-zoom"));
        tab.toggle_zoom();
        assert_eq!(zoom_label(&tab), Some("Zoom within quad-zoom"));
        tab.toggle_quadrant_zoom();
        assert_eq!(zoom_label(&tab), Some("Quad-zoom"));
        tab.toggle_quadrant_zoom();
        assert_eq!(zoom_label(&tab), None);
    }

    #[test]
    fn tab_zoom_icons_distinguish_outline_fill_and_nested_layers() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::Vec2::splat(12.0));
        let color = egui::Color32::WHITE;
        for (zoom, quad, expected) in [
            (None, None, 0),
            (Some(2), None, 1),
            (Some(4), Some(4), 1),
            (Some(2), Some(4), 2),
        ] {
            let mut tab = Tab::new(1, 2, "/tmp".into());
            tab.zoom = zoom;
            tab.quadrant_zoom = quad;
            let context = egui::Context::default();
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                paint_zoom_icon(ui.painter(), rect, &tab, color);
            });
            output.textures_delta.clear();
            let rectangles: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .collect();
            assert_eq!(rectangles.len(), expected);
            if zoom.is_some() {
                assert_eq!(rectangles[0].rect, rect);
                assert_eq!(
                    rectangles[0].fill,
                    if quad.is_some() {
                        egui::Color32::TRANSPARENT
                    } else {
                        color
                    }
                );
                assert_eq!(rectangles[0].stroke.width > 0.0, quad.is_some());
            }
            if expected == 2 {
                assert_eq!(rectangles[1].fill, color);
                assert_eq!(rectangles[1].rect, rect.shrink(3.0));
            }
        }
    }

    #[test]
    fn indefinite_progress_restarts_on_appearance_but_not_report_updates() {
        let now = Instant::now();
        let mut activity = Activity::default();
        activity.progress_reported(3, None, now);
        // The first displayed frame starts at the left, even after a hidden delay.
        let shown = now + Duration::from_secs(2);
        assert_eq!(activity.progress_offset(shown), 0.0);
        let middle = shown + Duration::from_millis(600);
        assert_eq!(activity.progress_offset(middle), 0.375);
        activity.progress_reported(3, None, middle);
        activity.progress_reported(2, None, middle);
        assert_eq!(activity.progress_offset(middle), 0.375);
        assert_eq!(
            activity.progress_offset(shown + Duration::from_millis(1200)),
            0.75
        );

        activity.reset_progress_animation(); // Tab/pane/window disappears.
        let shown = shown + Duration::from_secs(3);
        assert_eq!(activity.progress_offset(shown), 0.0);
        for (state, value) in [(1, Some(50)), (4, None), (0, None)] {
            assert_eq!(
                activity.progress_offset(shown + Duration::from_millis(600)),
                0.375
            );
            activity.progress_reported(state, value, shown);
            activity.progress_reported(3, None, shown);
            assert_eq!(activity.progress_offset(shown), 0.0);
        }
    }

    #[test]
    fn progress_retains_percentages_and_stops_on_removal_expiry_or_exit() {
        let now = Instant::now();
        let mut activity = Activity::default();
        for (state, value, expected) in [
            (1, Some(42), Some(42)),
            (2, Some(70), Some(70)),
            (2, None, None),
            (3, None, None),
            (4, None, Some(100)),
            (4, Some(30), Some(30)),
            (1, Some(255), Some(100)),
        ] {
            activity.progress_reported(state, value, now);
            assert_eq!(activity.progress().unwrap().percentage(), expected);
            assert_eq!(activity.progress().unwrap().state, state);
            assert_eq!(activity.deadline(), Some(now + PROGRESS_TIMEOUT));
        }
        activity.progress_reported(255, None, now);
        assert_eq!(activity.progress().unwrap().percentage(), Some(100));
        activity.command_started();
        activity.command_finished();
        assert!(activity.progress().is_some());
        activity.progress_reported(0, None, now);
        assert_eq!(activity.progress(), None);
        assert_eq!(activity.deadline(), None);
        activity.progress_reported(3, None, now);
        activity.expire(now + PROGRESS_TIMEOUT);
        assert_eq!(activity.progress(), None);
        activity.progress_reported(1, Some(42), now);
        activity.clear();
        assert_eq!(activity.progress(), None);
    }

    #[test]
    fn cursor_blink_restarts_visible_until_typing_stops() {
        let started = Instant::now();
        let interval = Duration::from_millis(600);
        assert_eq!(
            cursor_blink_phase(started, started),
            (true, started + interval)
        );
        assert_eq!(
            cursor_blink_phase(started, started + interval),
            (false, started + interval * 2),
        );

        let mut last_input = started;
        for ms in [650, 1_000, 1_400, 1_950] {
            last_input = started + Duration::from_millis(ms);
            let deadline = last_input + interval;
            assert_eq!(cursor_blink_phase(last_input, last_input), (true, deadline));
            // Even a draw just before the idle boundary keeps the original deadline.
            assert_eq!(
                cursor_blink_phase(last_input, deadline - Duration::from_nanos(1)),
                (true, deadline),
            );
        }
        let idle = last_input + interval;
        assert_eq!(
            cursor_blink_phase(last_input, idle),
            (false, idle + interval)
        );
        assert_eq!(
            cursor_blink_phase(last_input, idle + interval),
            (true, idle + interval * 2),
        );
    }

    #[test]
    fn focus_hint_expires_or_clears_on_input_without_rearming_on_hover() {
        let now = Instant::now();
        let mut hint = FocusHint::default();
        assert!(hint.focus(Some(1), now));
        assert!(hint.visible(1, now));
        let expiry = now + Duration::from_secs(2);
        assert_eq!(hint.deadline(now), Some(expiry));
        assert!(!hint.focus(Some(1), now + Duration::from_secs(1)));
        assert_eq!(hint.deadline(now), Some(expiry));
        assert!(!hint.visible(1, expiry));
        assert_eq!(hint.deadline(expiry), None);
        assert!(!hint.focus(Some(1), expiry));
        assert!(!hint.visible(1, expiry));

        assert!(hint.focus(Some(2), expiry));
        assert!(!hint.visible(1, expiry));
        assert!(hint.visible(2, expiry));
        hint.dismiss();
        assert!(!hint.focus(Some(2), expiry));
        assert!(!hint.visible(2, expiry));
        assert_eq!(hint.deadline(expiry), None);

        assert!(hint.focus(None, expiry));
        assert!(hint.focus(Some(2), expiry));
        assert!(hint.visible(2, expiry));
        assert!(hint.focus(None, expiry));
        assert!(!hint.visible(2, expiry));
        assert_eq!(hint.deadline(expiry), None);
    }

    #[test]
    fn work_count_distinguishes_commands_spinners_progress_and_waiting() {
        let now = Instant::now();
        let mut pane = Activity::default();
        assert!(!pane.command_started());
        assert!(pane.is_active());
        assert!(!pane.reported_active());
        for spinner in "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".chars() {
            for title in [
                spinner.to_string(),
                format!("{spinner} Fix a bug | project"),
            ] {
                pane.title_changed(&title);
                assert!(pane.reported_active());
            }
            for title in [format!("Fix a bug {spinner}"), format!("{spinner}filename")] {
                pane.title_changed(&title);
                assert!(!pane.reported_active());
            }
        }
        for (state, expected) in [(1, true), (2, false), (3, true), (4, false), (0, false)] {
            pane.progress_reported(state, None, now);
            assert_eq!(pane.reported_active(), expected);
        }
        pane.progress_reported(1, None, now);
        for title in ["[ ! ] Action Required", "[ . ] Action Required | project"] {
            pane.title_changed(title);
            assert!(!pane.reported_active());
        }
        pane.title_changed("[ ! ] Action Requiredness");
        assert!(pane.reported_active());
        pane.command_finished();
        assert!(!pane.reported_active());
        assert!(
            pane.is_active(),
            "command boundaries do not discard native progress"
        );
        pane.title_changed("normal title");
        assert!(
            !pane.reported_active(),
            "a stale report stays cleared until its next update"
        );
    }

    #[test]
    fn completion_is_per_pane_and_progress_expires_once_after_its_last_update() {
        let now = Instant::now();
        let mut first = Activity::default();
        let mut second = Activity::default();
        assert!(!first.command_finished());
        first.command_started();
        second.command_started();
        assert!(
            first.command_finished(),
            "another active pane must not suppress completion"
        );
        assert!(second.is_active());
        assert!(!first.command_finished());
        first.progress_reported(1, None, now);
        let updated = now + Duration::from_secs(10);
        first.progress_reported(4, None, updated);
        assert!(!first.expire(now + PROGRESS_TIMEOUT));
        assert_eq!(first.deadline(), Some(updated + PROGRESS_TIMEOUT));
        assert!(first.expire(updated + PROGRESS_TIMEOUT));
        assert!(!first.is_active());
        assert!(!first.expire(updated + PROGRESS_TIMEOUT));
        assert_eq!(first.deadline(), None);
        second.progress_reported(3, None, now);
        assert!(!second.command_finished());
        assert!(second.progress_reported(0, None, now));
        assert!(!second.clear());
    }

    #[test]
    fn directory_labels_disambiguate_paths_without_changing_shared_or_unique_names() {
        let cases: &[(&[&str], &[Option<&str>])] = &[
            (
                &["/one/project", "/two/other"],
                &[Some("project"), Some("other")],
            ),
            (
                &["/one/project", "/two/project", "/one/./project/"],
                &[
                    Some("one/project"),
                    Some("two/project"),
                    Some("one/project"),
                ],
            ),
            (
                &[
                    "/one/common/project",
                    "/two/common/project",
                    "/other/project",
                ],
                &[
                    Some("one/common/project"),
                    Some("two/common/project"),
                    Some("other/project"),
                ],
            ),
            (
                &["/project", "/nested/project"],
                &[Some("/project"), Some("nested/project")],
            ),
            (
                &["project", "nested/project"],
                &[Some("project"), Some("nested/project")],
            ),
            (
                &["/one/project", "one/project"],
                &[Some("/one/project"), Some("one/project")],
            ),
            (
                &["/one/../project", "/two/project"],
                &[Some("../project"), Some("two/project")],
            ),
            (
                &["/工作/项目", "/研究/项目"],
                &[Some("工作/项目"), Some("研究/项目")],
            ),
            (
                &["/one/project", "/one/./project/"],
                &[Some("project"), Some("project")],
            ),
            (&["/", "", "."], &[Some("/"), None, Some(".")]),
        ];
        for &(paths, expected) in cases {
            let entries: Vec<_> = paths
                .iter()
                .enumerate()
                .map(|(id, path)| (id as Id, Path::new(path)))
                .collect();
            let labels = directory_labels(entries.iter().copied());
            assert_eq!(
                labels,
                directory_labels(entries.iter().rev().copied()),
                "{paths:?}"
            );
            for (id, expected) in expected.iter().enumerate() {
                assert_eq!(
                    labels
                        .get(&(id as Id))
                        .map(|label| label.replace(std::path::MAIN_SEPARATOR, "/"))
                        .as_deref(),
                    *expected,
                    "{paths:?}, pane {id}",
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn directory_labels_preserve_windows_drive_and_unc_prefixes() {
        let paths = [
            r"C:\one\project",
            r"D:\one\project",
            r"\\server\share\project",
            r"\\other\share\project",
        ];
        let labels = directory_labels(
            paths
                .iter()
                .enumerate()
                .map(|(id, path)| (id as Id, Path::new(path))),
        );
        for (id, path) in paths.iter().enumerate() {
            assert_eq!(labels[&(id as Id)], *path);
        }
        let roots = directory_labels([(1, Path::new(r"C:\")), (2, Path::new(r"D:\"))]);
        assert_eq!(roots[&1], r"C:\");
        assert_eq!(roots[&2], r"D:\");
    }

    #[test]
    fn shared_directory_labels_follow_names_missing_values_and_focus() {
        assert_eq!(
            directory_name(Path::new("/one/project/")),
            Some("project".into())
        );
        assert_eq!(directory_name(Path::new("/")), Some("/".into()));
        assert_eq!(directory_name(Path::new("")), None);
        assert_eq!(
            common_label([Some("project"), Some("project")]),
            Some("project".into())
        );
        assert_eq!(
            common_label([Some("one/project"), Some("two/project")]),
            None
        );
        assert_eq!(common_label([Some("project"), None]), None);
        assert_eq!(common_label([Some("project"), Some("")]), None);
        assert_eq!(common_label([Some("project")]), Some("project".into()));
        assert_eq!(common_label([Some("")]), None);
        assert_eq!(common_label([]), None);
        let name = || Some("project".into());
        assert!(DirectoryLabel::new(name(), true, true, false).is_none());
        for (window_focused, show_focused) in [(false, false), (true, true)] {
            let label = DirectoryLabel::new(name(), true, window_focused, show_focused).unwrap();
            assert!(label.selected);
            assert_eq!(label.focused, window_focused);
            assert!(label.large(true));
            assert!(label.large(false));
            assert!(!label.shows_activity(true));
            assert!(!label.shows_flash(true, false));
            assert!(label.shows_flash(true, true));
        }
        let label = DirectoryLabel::new(name(), false, true, false).unwrap();
        assert!(label.large(true));
        assert!(label.shows_activity(true));
        assert!(!label.shows_activity(false));
        assert!(label.shows_flash(true, false));
    }

    #[test]
    fn shared_directory_labels_suppress_duplicates_without_consuming_the_focus_hint() {
        for (quad_selected, window_focused, peek, panel_selected, suppressed) in [
            (true, true, false, false, true),
            (true, true, false, true, false),
            (true, true, true, false, true),
            (true, true, true, true, true),
            (true, false, false, false, true),
            (true, false, false, true, true),
            (false, true, false, false, true),
            (false, true, false, true, true),
            (false, false, false, true, true),
        ] {
            let label =
                DirectoryLabel::new(Some("project".into()), quad_selected, window_focused, true)
                    .unwrap();
            assert_eq!(
                label.suppresses_panel_name(panel_selected, peek),
                suppressed,
                "quad_selected={quad_selected}, window_focused={window_focused}, peek={peek}, panel_selected={panel_selected}",
            );
        }
        for hint in [false, true] {
            assert_eq!(
                DirectoryLabel::new(Some("project".into()), true, true, hint).is_some(),
                hint,
            );
        }
    }

    #[test]
    fn pane_labels_only_center_within_their_endpoint_quadrant() {
        use rustty::config::Direction;
        let mut tab = Tab::new(1, 2, "/tmp".into());
        tab.split(3, 4, Direction::Right, "/tmp".into());
        tab.split(5, 6, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(7, 8, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(9, 10, Direction::Right, "/tmp".into());
        let label = DirectoryLabel::new(Some("project".into()), false, true, false).unwrap();
        for quadrant in [None, Some(10), None] {
            for pane in [3, 5, 7] {
                assert!(!label.centered_in_pane(&tab, pane, quadrant), "pane {pane}");
            }
            for pane in [2, 9] {
                assert_eq!(
                    label.centered_in_pane(&tab, pane, quadrant),
                    quadrant.is_some()
                );
            }
        }
        let selected = DirectoryLabel::new(Some("project".into()), true, true, true).unwrap();
        assert!(selected.centered_in_pane(&tab, tab.focused, None));
        assert!(selected.centered_in_pane(&tab, tab.focused, Some(10)));
    }

    #[test]
    fn completion_pulses_settle_and_tab_accents_preserve_contrast() {
        let now = Instant::now();
        let mut flash = CompletionFlash::default();
        assert_eq!(flash.next_repaint(now), None);
        flash.start(now);
        assert_eq!(flash.opacity(now), 0.0);
        assert_eq!(flash.opacity(now + Duration::from_millis(200)), 1.0);
        assert!((flash.opacity(now + Duration::from_millis(700)) - 0.5).abs() < 0.001);
        flash.start_if_idle(now + Duration::from_millis(100));
        assert_eq!(flash.opacity(now + FLASH_DURATION), 0.0);
        assert_eq!(flash.next_repaint(now + FLASH_DURATION), None);
        flash.start_if_idle(now + FLASH_DURATION);
        assert!(flash.next_repaint(now + FLASH_DURATION).is_some());
        flash.clear();
        assert_eq!(flash.next_repaint(now + FLASH_DURATION), None);
        assert_eq!(TabAccent::new(None, [10, 20, 30]).background, [10, 20, 30]);
        assert_eq!(
            TabAccent::new(Some([255, 200, 0]), [0; 3]).foreground,
            [0; 3]
        );
        assert_eq!(
            TabAccent::new(Some([20, 10, 90]), [255; 3]).foreground,
            [255; 3]
        );
        let accent = TabAccent::new(Some([255, 200, 0]), [0; 3]);
        assert_eq!(accent.frame_background([20; 3], 0.18), [62, 52, 16]);
        assert_eq!(
            TabAccent::new(None, [255, 200, 0]).frame_background([20; 3], 0.18),
            [62, 52, 16]
        );
        assert_eq!(accent.frame_background([240; 3], 0.18), [243, 233, 197]);
        assert_eq!(accent.frame_background([20; 3], 0.09), [41, 36, 18]);
        assert_eq!(accent.frame_background([20; 3], 0.0), [20; 3]);
        assert_eq!(accent.frame_background([20; 3], 1.0), accent.background);
    }
    #[test]
    fn unfocused_window_accents_are_nearly_neutral_and_restore_original_colors() {
        for (assigned, foreground) in [
            (None, [255; 3]),
            (Some([255, 200, 0]), [0; 3]),
            (Some([20, 10, 90]), [255; 3]),
            (Some([127, 128, 255]), [0; 3]),
            (Some([0; 3]), [255; 3]),
            (Some([255; 3]), [0; 3]),
        ] {
            let accent = TabAccent::new(assigned, [0, 100, 255]);
            assert_eq!(accent.for_window(true), accent);
            let muted = accent.for_window(false);
            let min = *muted.background.iter().min().unwrap();
            let max = *muted.background.iter().max().unwrap();
            assert!(max - min <= 13);
            assert_eq!(muted.foreground, foreground);
            let frame = muted.frame_background([20; 3], 0.18);
            assert!(frame.iter().max().unwrap() - frame.iter().min().unwrap() <= 3);
            assert_eq!(
                accent.for_window(true).background,
                assigned.unwrap_or([0, 100, 255])
            );
        }
    }
}
