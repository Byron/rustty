//! Shared tab, pane, and quadrant presentation rules from Ghostty Local.
use crate::workspace::Id;
use std::path::Path;
use std::time::{Duration, Instant};

pub const PROGRESS_TIMEOUT: Duration = Duration::from_secs(15);
pub const FLASH_DURATION: Duration = Duration::from_millis(1200);

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

/// Ghostty compares displayed basenames, not full paths or a common ancestor.
/// A missing directory prevents a shared label rather than hiding that pane.
pub fn common_directory_name<'a>(
    directories: impl IntoIterator<Item = Option<&'a Path>>,
) -> Option<String> {
    let mut names = directories
        .into_iter()
        .map(|path| path.and_then(directory_name));
    let first = names.next()??;
    names
        .all(|name| name.as_ref() == Some(&first))
        .then_some(first)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryLabel {
    pub name: String,
    pub focused: bool,
}
impl DirectoryLabel {
    /// A focused label is normally hidden only while its window is focused.
    /// `show_focused` reveals it during quadrant peek or the temporary focus hint.
    pub fn new(
        name: Option<String>,
        focused: bool,
        window_focused: bool,
        show_focused: bool,
    ) -> Option<Self> {
        let name = name.filter(|name| !name.is_empty())?;
        (!(focused && window_focused && !show_focused)).then_some(Self { name, focused })
    }

    pub fn large(&self, large_inactive: bool) -> bool {
        self.focused || large_inactive
    }

    pub fn shows_activity(&self, active: bool) -> bool {
        active && !self.focused
    }

    /// Quadrant labels include the focused quadrant; individual pane labels do not.
    pub fn shows_flash(&self, stopped: bool, include_focused: bool) -> bool {
        stopped && (include_focused || !self.focused)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabAccent {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
}
impl TabAccent {
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
    fn shared_directory_labels_follow_names_missing_values_and_focus() {
        assert_eq!(
            directory_name(Path::new("/one/project/")),
            Some("project".into())
        );
        assert_eq!(directory_name(Path::new("/")), Some("/".into()));
        assert_eq!(directory_name(Path::new("")), None);
        let common =
            |paths: &[Option<&str>]| common_directory_name(paths.iter().map(|p| p.map(Path::new)));
        assert_eq!(
            common(&[Some("/one/project"), Some("/two/project")]),
            Some("project".into())
        );
        assert_eq!(common(&[Some("/one"), Some("/two")]), None);
        assert_eq!(common(&[Some("/project"), None]), None);
        assert_eq!(common(&[Some("/project"), Some("")]), None);
        assert_eq!(common(&[Some("/project")]), Some("project".into()));
        assert_eq!(common(&[]), None);
        let name = || Some("project".into());
        assert!(DirectoryLabel::new(name(), true, true, false).is_none());
        for (window_focused, show_focused) in [(false, false), (true, true)] {
            let label = DirectoryLabel::new(name(), true, window_focused, show_focused).unwrap();
            assert!(label.focused);
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
    }
}
