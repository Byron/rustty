//! Workspace-wide, read-only pane projections. Membership changes only on selection.
use crate::deck::{PaneView, Target};
use egui::{Rect, Vec2};
use rustty::vt::agent::State as AgentState;
use std::{
    cmp::Reverse,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    State(AgentState),
    All,
}

pub const FILTERS: [(Filter, char, &str); 8] = [
    (Filter::State(AgentState::NeedsInput), 'n', "Needs input"),
    (Filter::State(AgentState::Working), 'r', "Running"),
    (Filter::State(AgentState::Done), 'd', "Done / ready"),
    (Filter::State(AgentState::Idle), 'i', "Idle"),
    (Filter::State(AgentState::Error), 'e', "Error"),
    (Filter::State(AgentState::Paused), 'p', "Paused"),
    (Filter::State(AgentState::Unknown), 'u', "Unknown"),
    (Filter::All, 'a', "All agents"),
];

impl Filter {
    pub fn matches(self, state: AgentState) -> bool {
        self == Self::All || self == Self::State(state)
    }
}

#[derive(Clone, Debug)]
pub struct Card {
    pub target: Target,
    pub number: usize,
    pub state: AgentState,
    pub rect: Rect,
    pub opacity: f32,
    from: Rect,
    to: Rect,
    from_opacity: f32,
    pub present: bool,
}

#[derive(Default)]
pub struct State {
    pub filter: Option<Filter>,
    pub cards: Vec<Card>,
    pub exiting: Option<(Target, Instant)>,
    pub number: usize,
    started: Option<Instant>,
    bounds: Option<Rect>,
    pub dim: f32,
    from_dim: f32,
}

impl State {
    pub fn active(&self) -> bool {
        self.filter.is_some() || !self.cards.is_empty()
    }

    pub fn contains(&self, pane: u64) -> bool {
        self.cards.iter().any(|card| card.target.pane == pane)
    }

    pub fn count(&self, filter: Filter, reports: &[PaneView]) -> usize {
        if self.filter == Some(filter) {
            self.cards.iter().filter(|card| card.present).count()
        } else {
            reports
                .iter()
                .filter(|report| filter.matches(report.state))
                .count()
        }
    }

    pub fn toggle(&mut self, filter: Filter, reports: &[PaneView], bounds: Rect, now: Instant) {
        if self.exiting.is_some() {
            return;
        }
        let next = (self.filter != Some(filter)).then_some(filter);
        if next.is_some() && !reports.iter().any(|report| filter.matches(report.state)) {
            return;
        }
        self.filter = next;
        self.number = 0;
        self.from_dim = self.dim;
        self.started = Some(now);
        self.bounds = Some(bounds);
        let mut members: Vec<_> = reports
            .iter()
            .filter(|report| next.is_some_and(|filter| filter.matches(report.state)))
            .collect();
        members.sort_by_key(|report| {
            (
                report.state != AgentState::Done,
                report.state == AgentState::Working,
                if report.state == AgentState::Working {
                    None
                } else {
                    Some(Reverse(report.last_activity))
                },
                report.target.pane,
            )
        });
        for card in &mut self.cards {
            card.from = card.rect;
            card.from_opacity = card.opacity;
            card.present = false;
            card.to = card.from.translate(Vec2::new(0.0, 30.0));
        }
        for (index, report) in members.iter().enumerate() {
            let to = card_rect(bounds, index, members.len());
            if let Some(card) = self
                .cards
                .iter_mut()
                .find(|card| card.target == report.target)
            {
                card.present = true;
                card.to = to;
                card.number = index + 1;
            } else {
                let from = to.translate(Vec2::new(0.0, 30.0));
                self.cards.push(Card {
                    target: report.target,
                    number: index + 1,
                    state: report.state,
                    rect: from,
                    opacity: 0.0,
                    from,
                    to,
                    from_opacity: 0.0,
                    present: true,
                });
            }
        }
    }

    pub fn select(&mut self, target: Target, now: Instant) {
        if self.exiting.is_none()
            && self
                .cards
                .iter()
                .any(|card| card.present && card.target == target)
        {
            for card in &mut self.cards {
                card.from = card.rect;
            }
            self.exiting = Some((target, now));
        }
    }

    pub fn numbered(&self, index: usize) -> Option<Target> {
        self.cards
            .iter()
            .find(|card| card.present && card.number == index + 1)
            .map(|card| card.target)
    }

    pub fn digit(&mut self, digit: usize, now: Instant) {
        self.number = self.number.saturating_mul(10).saturating_add(digit);
        let prefix = self.number.to_string();
        let longer = self.cards.iter().any(|card| {
            card.present
                && card.number > self.number
                && card.number.to_string().starts_with(&prefix)
        });
        if !longer {
            self.accept_number(now);
        }
    }

    pub fn accept_number(&mut self, now: Instant) {
        if let Some(target) = self
            .number
            .checked_sub(1)
            .and_then(|index| self.numbered(index))
        {
            self.select(target, now);
        }
        self.number = 0;
    }

    /// Returns the selected target only after it has left the viewport.
    pub fn update(
        &mut self,
        reports: &[PaneView],
        bounds: Rect,
        durations: [Duration; 2],
        now: Instant,
    ) -> (bool, Option<Target>) {
        let [duration, select_duration] = durations;
        self.cards
            .retain(|card| reports.iter().any(|report| report.target == card.target));
        for card in &mut self.cards {
            card.state = reports
                .iter()
                .find(|report| report.target == card.target)
                .unwrap()
                .state;
        }
        if let Some((target, started)) = self.exiting {
            if !self.cards.iter().any(|card| card.target == target) {
                self.exiting = None;
            } else {
                let phase = phase(started, now, select_duration);
                for card in &mut self.cards {
                    if card.target == target {
                        card.rect = card.from.translate(Vec2::new(
                            (bounds.right() - card.from.left() + 16.0) * smooth(phase),
                            0.0,
                        ));
                    } else {
                        card.opacity = 1.0 - phase;
                    }
                }
                return (phase < 1.0, (phase >= 1.0).then_some(target));
            }
        }
        if self.bounds != Some(bounds) {
            self.cards.sort_by_key(|card| card.number);
            let count = self.cards.iter().filter(|card| card.present).count();
            for (index, card) in self
                .cards
                .iter_mut()
                .filter(|card| card.present)
                .enumerate()
            {
                card.from = card.rect;
                card.from_opacity = card.opacity;
                card.to = card_rect(bounds, index, count);
            }
            self.from_dim = self.dim;
            self.started = Some(now);
            self.bounds = Some(bounds);
        }
        let phase = self
            .started
            .map_or(1.0, |started| phase(started, now, duration));
        let mix = smooth(phase);
        let dim = f32::from(self.filter.is_some());
        self.dim = self.from_dim + (dim - self.from_dim) * mix;
        let mut moving = phase < 1.0;
        for card in &mut self.cards {
            let center = card.from.center().lerp(card.to.center(), mix);
            let size =
                (card.from.size() + (card.to.size() - card.from.size()) * mix).min(bounds.size());
            card.rect = Rect::from_center_size(center, size);
            // Keep previews inside the viewport and above the selectors.
            card.rect = card.rect.translate(Vec2::new(
                (bounds.left() - card.rect.left()).max(0.0)
                    - (card.rect.right() - bounds.right()).max(0.0),
                (bounds.top() - card.rect.top()).max(0.0)
                    - (card.rect.bottom() - bounds.bottom()).max(0.0),
            ));
            card.opacity = card.from_opacity + (f32::from(card.present) - card.from_opacity) * mix;
        }
        if phase == 1.0 {
            self.cards.retain(|card| card.present);
        }
        if self.cards.is_empty() && self.filter.is_some() {
            self.filter = None;
            self.from_dim = self.dim;
            self.started = Some(now);
            moving = true;
        }
        (moving, None)
    }
}

pub fn card_rect(bounds: Rect, index: usize, count: usize) -> Rect {
    let cols = ((count as f32 * bounds.width() / bounds.height().max(1.0) / 1.6)
        .sqrt()
        .ceil() as usize)
        .clamp(1, count.max(1));
    let rows = count.div_ceil(cols).max(1);
    let cell = bounds.size() / Vec2::new(cols as f32, rows as f32);
    let cell = Rect::from_min_size(
        bounds.min + cell * Vec2::new((index % cols) as f32, (index / cols) as f32),
        cell,
    )
    .shrink(8.0);
    let size = cell.size() * if count == 1 { 0.8 } else { 1.0 };
    Rect::from_center_size(cell.center(), size)
}

fn phase(start: Instant, now: Instant, duration: Duration) -> f32 {
    if duration.is_zero() {
        1.0
    } else {
        (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32()).min(1.0)
    }
}
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;
    fn report(pane: u64, state: AgentState) -> PaneView {
        PaneView {
            target: Target {
                pane,
                generation: 1,
            },
            state,
            last_activity: None,
            label: String::new(),
            unseen: false,
            remote: false,
        }
    }
    fn update(state: &mut State, reports: &[PaneView], now: Instant) -> (bool, Option<Target>) {
        state.update(
            reports,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0)),
            [Duration::ZERO; 2],
            now,
        )
    }
    #[test]
    fn filters_cover_every_state_and_reconcile_without_losing_shared_panels() {
        let now = Instant::now();
        let mut reports: Vec<_> = FILTERS[..7]
            .iter()
            .enumerate()
            .map(|(i, (filter, _, _))| {
                let Filter::State(state) = filter else {
                    unreachable!()
                };
                report(i as u64 + 1, *state)
            })
            .collect();
        let mut state = State::default();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        for (filter, _, _) in FILTERS {
            assert!(state.count(filter, &reports) > 0);
        }
        state.toggle(Filter::State(AgentState::Idle), &reports, bounds, now);
        update(&mut state, &reports, now);
        let idle = state.cards[0].target;
        reports
            .iter_mut()
            .find(|report| report.target == idle)
            .unwrap()
            .state = AgentState::Done;
        update(&mut state, &reports, now);
        assert_eq!(state.cards[0].state, AgentState::Done);
        assert_eq!(state.numbered(0), Some(idle));
        let from = state.cards[0].rect;
        state.toggle(Filter::All, &reports, bounds, now);
        assert_eq!(
            state
                .cards
                .iter()
                .find(|card| card.target == idle)
                .unwrap()
                .from,
            from
        );
        update(&mut state, &reports, now);
        assert_eq!(state.cards.len(), 7);
        state.toggle(Filter::All, &reports, bounds, now);
        update(&mut state, &reports, now);
        assert!(!state.active());
    }
    #[test]
    fn newest_completion_first_and_selection_waits_for_flyout() {
        let now = Instant::now();
        let mut reports = vec![report(1, AgentState::Done), report(2, AgentState::Done)];
        reports[0].last_activity = Some(now - Duration::from_secs(10));
        reports[1].last_activity = Some(now);
        let mut state = State::default();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        state.toggle(Filter::All, &reports, bounds, now);
        update(&mut state, &reports, now);
        assert_eq!(state.numbered(0), Some(reports[1].target));
        state.select(reports[1].target, now);
        let (_, selected) = state.update(
            &reports,
            bounds,
            [Duration::ZERO, Duration::from_secs(1)],
            now + Duration::from_millis(500),
        );
        assert_eq!(selected, None);
        let (_, selected) = update(&mut state, &reports, now + Duration::from_secs(1));
        assert_eq!(selected, Some(reports[1].target));
        assert!(
            state
                .cards
                .iter()
                .find(|card| card.target == reports[1].target)
                .unwrap()
                .rect
                .left()
                > bounds.right()
        );
    }
    #[test]
    fn sparse_layouts_leave_a_background() {
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        for count in [1, 2, 7] {
            for index in 0..count {
                let card = card_rect(bounds, index, count);
                assert!(bounds.contains_rect(card));
            }
        }
        assert!(card_rect(bounds, 0, 1).width() < bounds.width());
    }

    #[test]
    fn preview_slots_fill_square_cells_with_small_gutters() {
        let bounds = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::splat(1500.0));
        let first = card_rect(bounds, 0, 8);
        assert_eq!(first.size(), Vec2::splat(484.0));
        assert_eq!(card_rect(bounds, 1, 8).left() - first.right(), 16.0);
        assert_eq!(card_rect(bounds, 3, 8).top() - first.bottom(), 16.0);
        assert!(bounds.contains_rect(card_rect(bounds, 7, 8)));
    }

    #[test]
    fn multi_digit_labels_remain_keyboard_selectable() {
        let now = Instant::now();
        let reports: Vec<_> = (1..=12).map(|id| report(id, AgentState::Idle)).collect();
        let mut state = State::default();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        state.toggle(Filter::All, &reports, bounds, now);
        update(&mut state, &reports, now);
        state.digit(1, now);
        assert_eq!(state.number, 1);
        assert!(state.exiting.is_none());
        state.digit(2, now);
        assert_eq!(state.exiting.unwrap().0.pane, 12);
        state.exiting = None;
        state.digit(1, now);
        state.accept_number(now);
        assert_eq!(state.exiting.unwrap().0.pane, 1);
    }

    #[test]
    fn closed_or_replaced_targets_cannot_be_selected() {
        let now = Instant::now();
        let mut reports = vec![report(1, AgentState::Working)];
        let mut state = State::default();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        state.toggle(Filter::All, &reports, bounds, now);
        update(&mut state, &reports, now);
        state.select(reports[0].target, now);
        reports[0].target.generation += 1;
        assert_eq!(update(&mut state, &reports, now).1, None);
        assert_eq!(state.numbered(0), None);
    }
}
