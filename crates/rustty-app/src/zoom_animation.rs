//! Animate presentation geometry while terminal grids stay at either endpoint.
use crate::workspace::{Id, Rect, Tab};
use rustty::config::Config;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pane {
    pub id: Id,
    pub layout: egui::Rect,
    /// Retained neighbors keep the label placement of their own endpoint.
    pub label_quadrant: Option<Id>,
    pub rect: egui::Rect,
    pub opacity: f32,
    pub background_opacity: f32,
    pub moving: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct View {
    zoom: Option<Id>,
    quadrant: Option<Id>,
    peek: bool,
    panes: Vec<(Id, egui::Rect)>,
}

impl View {
    fn mode(&self) -> (Option<Id>, Option<Id>, bool) {
        (self.zoom, self.quadrant, self.peek)
    }

    fn label_quadrant(&self) -> Option<Id> {
        self.quadrant
            .filter(|_| self.peek || self.zoom == self.quadrant)
    }

    fn panes(&self) -> Vec<Pane> {
        self.panes
            .iter()
            .map(|&(id, rect)| Pane {
                id,
                layout: rect,
                label_quadrant: self.label_quadrant(),
                rect,
                opacity: 1.0,
                background_opacity: 1.0,
                moving: false,
            })
            .collect()
    }
}

struct Snapshot {
    tab: Id,
    content: Rect,
    topology: Vec<(Id, Rect)>,
    cell_height: f32,
    minimum: [f32; 2],
    scale: f32,
    view: View,
}

impl Snapshot {
    fn compatible(&self, next: &Self) -> bool {
        self.tab == next.tab
            && self.content == next.content
            && self.cell_height == next.cell_height
            && self.minimum == next.minimum
            && self.scale == next.scale
    }
}

struct Transition {
    source: Vec<Pane>,
    source_view: Option<View>,
    source_mode: (Option<Id>, Option<Id>, bool),
    target: View,
    phase: f32,
    direction: f32,
    duration: Duration,
    background_mix: f32,
}

impl Transition {
    fn display_zoom(&self, phase: f32) -> (Option<Id>, Option<Id>, bool) {
        if phase < 0.5 {
            self.source_mode
        } else {
            self.target.mode()
        }
    }

    fn phase(&self, started: Instant, now: Instant) -> f32 {
        (self.phase
            + self.direction * now.saturating_duration_since(started).as_secs_f32()
                / self.duration.as_secs_f32())
        .clamp(0.0, 1.0)
    }

    fn finished(&self, phase: f32) -> bool {
        if self.direction > 0.0 {
            phase >= 1.0
        } else {
            phase <= 0.0
        }
    }

    fn panes(&self, phase: f32) -> Vec<Pane> {
        let position = smoothstep(phase);
        // Reach the background mix while neighbors are still exposed by the slide.
        let background_fade = smoothstep((phase * 2.0).min(1.0));
        let opacity = if phase < 0.5 {
            smoothstep(1.0 - phase * 2.0)
        } else {
            smoothstep(phase * 2.0 - 1.0)
        };
        let sliding = self
            .source
            .iter()
            .any(|source| self.target.panes.iter().any(|(id, _)| *id == source.id));
        let mut panes = self
            .source
            .iter()
            .filter_map(|source| {
                let target = self.target.panes.iter().find(|(id, _)| *id == source.id);
                match target {
                    Some((_, layout)) => Some(Pane {
                        layout: if phase < 0.5 { source.layout } else { *layout },
                        label_quadrant: if phase < 0.5 {
                            source.label_quadrant
                        } else {
                            self.target.label_quadrant()
                        },
                        rect: interpolate(source.rect, *layout, position),
                        opacity: if phase < 0.5 {
                            source.opacity * opacity
                        } else {
                            opacity
                        },
                        background_opacity: if phase < 0.5 {
                            source.background_opacity
                                + (1.0 - source.background_opacity) * (1.0 - opacity)
                        } else {
                            1.0
                        },
                        moving: true,
                        ..*source
                    }),
                    None if sliding && !source.moving => Some(Pane {
                        opacity: source.opacity
                            + (1.0 - self.background_mix - source.opacity) * background_fade,
                        background_opacity: source.background_opacity
                            + (1.0 - self.background_mix - source.background_opacity)
                                * background_fade,
                        ..*source
                    }),
                    None if phase < 0.5 => Some(Pane {
                        opacity: source.opacity * opacity,
                        background_opacity: source.background_opacity * opacity,
                        ..*source
                    }),
                    None => None,
                }
            })
            .collect::<Vec<_>>();
        if sliding || phase >= 0.5 {
            panes.extend(self.target.panes.iter().filter_map(|&(id, layout)| {
                if self.source.iter().any(|source| source.id == id) {
                    return None;
                }
                let opacity = if sliding {
                    1.0 - self.background_mix * smoothstep(((1.0 - phase) * 2.0).min(1.0))
                } else {
                    opacity
                };
                Some(Pane {
                    id,
                    layout,
                    label_quadrant: self.target.label_quadrant(),
                    rect: layout,
                    opacity,
                    background_opacity: opacity,
                    moving: false,
                })
            }));
        }
        // The enlarging pane or quadrant covers its neighbors throughout the slide.
        panes.sort_by_key(|pane| pane.moving);
        panes
    }
}

#[derive(Default)]
pub struct State {
    pub started: Option<Instant>,
    snapshot: Option<Snapshot>,
    transition: Option<Transition>,
    display_zoom: Option<(Option<Id>, Option<Id>, bool)>,
}

impl State {
    pub fn is_running(&self) -> bool {
        self.transition.is_some()
    }

    pub fn display_zoom(&self) -> Option<(Option<Id>, Option<Id>, bool)> {
        self.display_zoom
    }

    pub fn finish(&mut self) {
        self.transition = None;
        self.started = None;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        tab: &Tab,
        peek: bool,
        content: Rect,
        cell_height: f32,
        minimum: [f32; 2],
        scale: f32,
        config: &Config,
        enabled: bool,
        now: Instant,
    ) -> Vec<Pane> {
        let next = Snapshot {
            tab: tab.id,
            content,
            topology: tab.root.layout(content),
            cell_height,
            minimum,
            scale,
            view: View {
                zoom: tab.zoom,
                quadrant: tab.quadrant_zoom,
                peek,
                panes: tab
                    .layout_with_minimum(content, peek, cell_height, minimum)
                    .into_iter()
                    .map(|(id, rect)| {
                        (
                            id,
                            egui::Rect::from_min_size(
                                egui::pos2(rect.x, rect.y),
                                egui::vec2(rect.width, rect.height),
                            )
                            .shrink(1.0),
                        )
                    })
                    .collect(),
            },
        };
        let previous = self.snapshot.take();
        let phase = self
            .transition
            .as_ref()
            .zip(self.started)
            .map(|(transition, started)| transition.phase(started, now));
        if self
            .transition
            .as_ref()
            .zip(phase)
            .is_some_and(|(transition, phase)| transition.finished(phase))
        {
            self.finish();
        }
        if !enabled
            || previous.as_ref().is_none_or(|old| {
                !old.compatible(&next)
                    || old.view.mode() == next.view.mode()
                        && (old.topology != next.topology || old.view.panes != next.view.panes)
            })
        {
            self.finish();
        } else if let Some(previous) = &previous
            && previous.view != next.view
        {
            let duration = if previous.view.peek || next.view.peek {
                config.quad_peek_animation_duration
            } else if previous.view.quadrant.is_some()
                && previous.view.quadrant == next.view.quadrant
            {
                config.zoom_from_quad_animation_duration
            } else if previous.view.quadrant.is_some() || next.view.quadrant.is_some() {
                config.quad_zoom_animation_duration
            } else {
                config.zoom_animation_duration
            };
            let reverse = self.transition.as_ref().and_then(|transition| {
                if transition.source_view.as_ref() == Some(&next.view) {
                    Some(-1.0)
                } else if transition.target == next.view {
                    Some(1.0)
                } else {
                    None
                }
            });
            if duration.is_zero() {
                self.finish();
            } else if let Some(direction) = reverse {
                let transition = self.transition.as_mut().unwrap();
                transition.phase = phase.unwrap();
                transition.direction = direction;
                transition.duration = duration;
                self.started = Some(now);
            } else if self.transition.is_some() || previous.view.panes != next.view.panes {
                let (source, source_view, source_mode) = if let Some(transition) = &self.transition
                {
                    (
                        transition.panes(phase.unwrap()),
                        None,
                        transition.display_zoom(phase.unwrap()),
                    )
                } else {
                    (
                        previous.view.panes(),
                        Some(previous.view.clone()),
                        previous.view.mode(),
                    )
                };
                self.transition = Some(Transition {
                    source: source
                        .into_iter()
                        .filter(|pane| tab.contains(pane.id))
                        .collect(),
                    source_view,
                    source_mode,
                    target: next.view.clone(),
                    phase: 0.0,
                    direction: 1.0,
                    duration,
                    background_mix: config.zoom_animation_background_mix,
                });
                self.started = Some(now);
            }
        }
        let panes = self.transition.as_ref().map_or_else(
            || next.view.panes(),
            |transition| transition.panes(transition.phase(self.started.unwrap(), now)),
        );
        self.display_zoom = Some(self.transition.as_ref().map_or_else(
            || next.view.mode(),
            |transition| transition.display_zoom(transition.phase(self.started.unwrap(), now)),
        ));
        self.snapshot = Some(next);
        panes
    }
}

fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

fn interpolate(from: egui::Rect, to: egui::Rect, position: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        from.min + (to.min - from.min) * position,
        from.max + (to.max - from.max) * position,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::{Direction, Modifiers};
    use std::path::PathBuf;

    const ZOOM_DURATION: Duration = Duration::from_millis(250);
    const PEEK_DURATION: Duration = Duration::from_millis(100);

    const CONTENT: Rect = Rect {
        x: 10.0,
        y: 20.0,
        width: 1000.0,
        height: 800.0,
    };

    fn quadrants() -> Tab {
        let directory = PathBuf::from("/tmp");
        let mut tab = Tab::new(1, 2, directory.clone());
        tab.split(3, 4, Direction::Right, directory.clone());
        tab.split(5, 6, Direction::Down, directory.clone());
        tab.focus(2);
        tab.split(7, 8, Direction::Down, directory.clone());
        tab.focus(2);
        tab.split(9, 10, Direction::Right, directory);
        tab
    }

    fn update(state: &mut State, tab: &Tab, peek: bool, now: Instant) -> Vec<Pane> {
        state.update(
            tab,
            peek,
            CONTENT,
            16.0,
            [0.0; 2],
            1.0,
            &Config::default(),
            true,
            now,
        )
    }

    fn pane(panes: &[Pane], id: Id) -> Pane {
        *panes.iter().find(|pane| pane.id == id).unwrap()
    }

    #[test]
    fn zoom_prepares_only_endpoint_sizes_and_hides_text_at_midpoint() {
        let now = Instant::now();
        let mut state = State::default();
        let mut tab = quadrants();
        let source = update(&mut state, &tab, false, now);
        tab.toggle_quadrant_zoom();
        let start = update(&mut state, &tab, false, now);
        let id = tab.focused;
        assert_eq!(pane(&start, id).layout, pane(&source, id).layout);
        assert!(state.is_running());
        assert!(
            start
                .iter()
                .skip_while(|pane| !pane.moving)
                .all(|pane| pane.moving)
        );
        let before = update(&mut state, &tab, false, now + Duration::from_millis(124));
        assert_eq!(state.display_zoom(), Some((None, None, false)));
        let middle = update(&mut state, &tab, false, now + Duration::from_millis(125));
        assert_eq!(
            state.display_zoom(),
            Some((tab.zoom, tab.quadrant_zoom, false))
        );
        let after = update(&mut state, &tab, false, now + Duration::from_millis(200));
        let end = update(&mut state, &tab, false, now + ZOOM_DURATION);
        assert!(!state.is_running());
        assert_eq!(pane(&before, id).layout, pane(&source, id).layout);
        assert_eq!(pane(&middle, id).layout, pane(&end, id).layout);
        assert_eq!(pane(&after, id).layout, pane(&end, id).layout);
        assert_ne!(pane(&before, id).rect, pane(&before, id).layout);
        assert_ne!(pane(&after, id).rect, pane(&after, id).layout);
        assert!(
            middle
                .iter()
                .filter(|pane| pane.moving)
                .all(|pane| pane.opacity == 0.0)
        );
        assert!(
            middle
                .iter()
                .filter(|pane| pane.moving)
                .all(|pane| pane.background_opacity == 1.0)
        );
        assert_eq!(end.len(), 2);
    }

    #[test]
    fn pane_zoom_inside_quad_and_peek_use_their_endpoint_layouts() {
        let now = Instant::now();
        let mut state = State::default();
        let mut tab = quadrants();
        tab.toggle_quadrant_zoom();
        update(&mut state, &tab, false, now);
        tab.toggle_zoom();
        update(&mut state, &tab, false, now);
        let nested = update(&mut state, &tab, false, now + ZOOM_DURATION);
        assert_eq!(nested.len(), 1);
        let peek = tab
            .begin_peek(Modifiers {
                super_key: true,
                ..Modifiers::default()
            })
            .unwrap();
        let start = now + ZOOM_DURATION;
        update(&mut state, &tab, true, start);
        let middle = update(&mut state, &tab, true, start + PEEK_DURATION / 2);
        assert!(
            middle
                .iter()
                .filter(|pane| pane.moving)
                .all(|pane| pane.opacity == 0.0)
        );
        assert_eq!(middle.len(), 5);
        assert_eq!(middle.iter().filter(|pane| pane.moving).count(), 1);
        update(&mut state, &tab, true, start + PEEK_DURATION);
        assert!(!state.is_running());
        tab.finish_peek(peek);
        let start = start + PEEK_DURATION;
        update(&mut state, &tab, false, start);
        let restored = update(&mut state, &tab, false, start + PEEK_DURATION);
        assert!(!state.is_running());
        assert_eq!(restored, nested);
    }

    #[test]
    fn nested_zoom_keeps_background_labels_centered_in_both_directions() {
        use crate::presentation::DirectoryLabel;
        let mut quad = quadrants();
        quad.toggle_quadrant_zoom();
        let mut nested = quad.clone();
        nested.toggle_zoom();
        let label = DirectoryLabel::new(Some("project".into()), false, true, false).unwrap();
        for (from, to) in [(&quad, &nested), (&nested, &quad)] {
            for millis in [0, 62, 124, 125, 188, 249] {
                let now = Instant::now();
                let mut state = State::default();
                update(&mut state, from, false, now);
                update(&mut state, to, false, now);
                let sampled = now + Duration::from_millis(millis);
                let panes = update(&mut state, to, false, sampled);
                let backgrounds: Vec<_> = panes.iter().filter(|pane| !pane.moving).collect();
                assert!(!backgrounds.is_empty());
                for pane in backgrounds {
                    assert!(
                        label.centered_in_pane(to, pane.id, pane.label_quadrant),
                        "pane {} at {millis} ms, zoom {:?}",
                        pane.id,
                        to.zoom,
                    );
                }
                assert_eq!(update(&mut state, from, false, sampled), panes);
            }
        }
    }

    #[test]
    fn stationary_backgrounds_use_the_same_mix_throughout_zoom_in_and_out() {
        let now = Instant::now();
        let overview = quadrants();
        let mut quad = overview.clone();
        quad.toggle_quadrant_zoom();
        let mut fullscreen = overview.clone();
        fullscreen.toggle_zoom();
        let mut nested = quad.clone();
        nested.toggle_zoom();
        for (from, to) in [
            (&overview, &quad),
            (&overview, &fullscreen),
            (&quad, &nested),
        ] {
            for mix in [0.0, 0.5, 1.0] {
                let mut config = Config::default();
                config.zoom_animation_background_mix = mix;
                let mut entering = State::default();
                entering.update(
                    from, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now,
                );
                entering.update(to, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now);
                let mut leaving = State::default();
                leaving.update(to, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now);
                leaving.update(
                    from, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now,
                );
                let entering = entering.transition.as_ref().unwrap();
                let leaving = leaving.transition.as_ref().unwrap();
                for phase in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    let forward = entering.panes(phase);
                    let backward = leaving.panes(1.0 - phase);
                    assert_eq!(forward.len(), entering.source.len());
                    assert_eq!(backward.len(), entering.source.len());
                    assert!(
                        forward
                            .iter()
                            .skip_while(|pane| !pane.moving)
                            .all(|pane| pane.moving)
                    );
                    for background in forward.iter().filter(|pane| !pane.moving) {
                        let reversed = pane(&backward, background.id);
                        assert!(!reversed.moving);
                        assert_eq!(background.rect, reversed.rect);
                        assert_eq!(background.layout, background.rect);
                        assert_eq!(reversed.layout, reversed.rect);
                        assert_eq!(background.label_quadrant, from.quadrant_zoom);
                        assert_eq!(background.label_quadrant, reversed.label_quadrant);
                        assert_eq!(background.opacity, reversed.opacity);
                        assert_eq!(background.background_opacity, reversed.background_opacity);
                        assert_eq!(background.opacity, background.background_opacity);
                        assert!((1.0 - mix..=1.0).contains(&background.opacity));
                        match phase {
                            0.0 => assert_eq!(background.opacity, 1.0),
                            0.25 => assert_eq!(background.opacity, 1.0 - mix / 2.0),
                            _ => assert_eq!(background.opacity, 1.0 - mix),
                        }
                    }
                    for moving in forward.iter().filter(|pane| pane.moving) {
                        let source = entering
                            .source
                            .iter()
                            .find(|pane| pane.id == moving.id)
                            .unwrap();
                        let target = entering
                            .target
                            .panes
                            .iter()
                            .find(|(id, _)| *id == moving.id)
                            .unwrap();
                        assert_eq!(
                            moving.layout,
                            if phase < 0.5 { source.layout } else { target.1 }
                        );
                        if phase == 0.5 {
                            assert_eq!(moving.opacity, 0.0);
                            assert_eq!(moving.background_opacity, 1.0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn switching_unrelated_fullscreen_panes_keeps_the_midpoint_fade() {
        let now = Instant::now();
        let mut tab = quadrants();
        tab.zoom = Some(2);
        let mut state = State::default();
        update(&mut state, &tab, false, now);
        tab.zoom = Some(5);
        update(&mut state, &tab, false, now);
        let transition = state.transition.as_ref().unwrap();
        for (phase, id, opacity) in [(0.25, 2, 0.5), (0.5, 5, 0.0), (0.75, 5, 0.5)] {
            let panes = transition.panes(phase);
            assert_eq!(panes.len(), 1);
            assert_eq!(panes[0].id, id);
            assert_eq!(panes[0].opacity, opacity);
            assert_eq!(panes[0].background_opacity, opacity);
        }
    }

    #[test]
    fn each_zoom_kind_uses_its_configured_duration_in_both_directions() {
        let mut config = Config::default();
        config.quad_zoom_animation_duration = Duration::from_millis(600);
        config.quad_peek_animation_duration = Duration::from_millis(320);
        config.zoom_animation_duration = Duration::from_millis(800);
        config.zoom_from_quad_animation_duration = Duration::from_millis(1000);
        let update = |state: &mut State, tab: &Tab, peek, now| {
            state.update(tab, peek, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now)
        };
        let overview = quadrants();
        let mut quad = overview.clone();
        quad.toggle_quadrant_zoom();
        let mut nested = quad.clone();
        nested.toggle_zoom();
        let mut pane = overview.clone();
        pane.toggle_zoom();
        let mut peek = nested.clone();
        peek.begin_peek(Modifiers {
            super_key: true,
            ..Modifiers::default()
        })
        .unwrap();
        for (from, to, duration) in [
            (
                (&overview, false),
                (&quad, false),
                config.quad_zoom_animation_duration,
            ),
            (
                (&nested, false),
                (&peek, true),
                config.quad_peek_animation_duration,
            ),
            (
                (&overview, false),
                (&pane, false),
                config.zoom_animation_duration,
            ),
            (
                (&quad, false),
                (&nested, false),
                config.zoom_from_quad_animation_duration,
            ),
        ] {
            for (from, to) in [(from, to), (to, from)] {
                let now = Instant::now();
                let mut state = State::default();
                update(&mut state, from.0, from.1, now);
                update(&mut state, to.0, to.1, now);
                assert_eq!(state.transition.as_ref().unwrap().duration, duration);
                let middle = update(&mut state, to.0, to.1, now + duration / 2);
                assert!(
                    middle
                        .iter()
                        .filter(|pane| pane.moving)
                        .all(|pane| pane.opacity == 0.0)
                );
                update(
                    &mut state,
                    to.0,
                    to.1,
                    now + duration - Duration::from_millis(1),
                );
                assert!(state.is_running());
                let end = update(&mut state, to.0, to.1, now + duration);
                assert!(!state.is_running());
                assert!(
                    end.iter()
                        .all(|pane| pane.rect == pane.layout && pane.opacity == 1.0)
                );
            }
        }
    }

    #[test]
    fn zero_duration_settles_immediately_even_when_reversing() {
        let now = Instant::now();
        let mut tab = quadrants();
        let mut state = State::default();
        let original = update(&mut state, &tab, false, now);
        tab.toggle_zoom();
        update(&mut state, &tab, false, now);
        tab.toggle_zoom();
        let mut config = Config::default();
        config.zoom_animation_duration = Duration::ZERO;
        let reversed = state.update(
            &tab, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now,
        );
        assert_eq!(reversed, original);
        assert!(!state.is_running());
        tab.toggle_zoom();
        let zoomed = state.update(
            &tab, false, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now,
        );
        assert!(!state.is_running());
        assert_eq!(zoomed.len(), 1);
        assert_eq!(zoomed[0].rect, zoomed[0].layout);
        assert_eq!(zoomed[0].opacity, 1.0);
    }

    #[test]
    fn reversal_preserves_position_opacity_and_endpoint_grid() {
        let now = Instant::now();
        let mut config = Config::default();
        config.zoom_animation_duration = Duration::from_millis(600);
        let update = |state: &mut State, tab: &Tab, peek, now| {
            state.update(tab, peek, CONTENT, 16.0, [0.0; 2], 1.0, &config, true, now)
        };
        for elapsed in [Duration::from_millis(180), Duration::from_millis(420)] {
            let mut tab = quadrants();
            let mut state = State::default();
            let original = update(&mut state, &tab, false, now);
            tab.toggle_zoom();
            update(&mut state, &tab, false, now);
            let reverse_at = now + elapsed;
            let forward = update(&mut state, &tab, false, reverse_at);
            tab.toggle_zoom();
            let reversed = update(&mut state, &tab, false, reverse_at);
            assert_eq!(reversed, forward);
            assert_eq!(
                state.transition.as_ref().unwrap().duration,
                config.zoom_animation_duration
            );
            let back = update(&mut state, &tab, false, reverse_at + elapsed);
            assert_eq!(back, original);
            assert!(!state.is_running());
        }
    }

    #[test]
    fn interrupted_peek_keeps_visible_geometry_and_fades_when_target_changes() {
        let now = Instant::now();
        let mut tab = quadrants();
        tab.toggle_quadrant_zoom();
        let mut state = State::default();
        update(&mut state, &tab, false, now);
        let mut peek = tab
            .begin_peek(Modifiers {
                super_key: true,
                ..Modifiers::default()
            })
            .unwrap();
        update(&mut state, &tab, true, now);
        let interrupted = now + Duration::from_millis(70);
        let before = update(&mut state, &tab, true, interrupted);
        peek.target = 5;
        let target = tab.finish_peek(peek);
        tab.focus(target);
        let after = update(&mut state, &tab, false, interrupted);
        for old in &before {
            let next = pane(&after, old.id);
            assert_eq!(next.rect, old.rect);
            assert_eq!(next.layout, old.layout);
            assert_eq!(next.label_quadrant, old.label_quadrant);
            assert_eq!(next.opacity, old.opacity);
            assert_eq!(next.background_opacity, old.background_opacity);
        }
        let middle = update(&mut state, &tab, false, interrupted + PEEK_DURATION / 2);
        for old in before
            .iter()
            .filter(|pane| pane.moving && pane.id != target)
        {
            assert!(middle.iter().all(|pane| pane.id != old.id));
        }
        let end = update(&mut state, &tab, false, interrupted + PEEK_DURATION);
        assert!(!state.is_running());
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].id, 5);
    }

    #[test]
    fn closing_a_quad_pane_animates_automatic_unzoom() {
        for closed in [2, 9] {
            let now = Instant::now();
            let mut tab = quadrants();
            tab.toggle_quadrant_zoom();
            let mut state = State::default();
            let source = update(&mut state, &tab, false, now);
            assert!(tab.close(closed));
            assert_eq!((tab.zoom, tab.quadrant_zoom), (None, None));
            let survivor = if closed == 2 { 9 } else { 2 };
            let start = update(&mut state, &tab, false, now);
            assert!(state.is_running());
            assert!(start.iter().all(|pane| pane.id != closed));
            assert_eq!(pane(&start, survivor).rect, pane(&source, survivor).rect);
            assert_eq!(
                pane(&start, survivor).layout,
                pane(&source, survivor).layout
            );
            let middle = update(&mut state, &tab, false, now + ZOOM_DURATION / 2);
            assert!(middle.iter().all(|pane| pane.id != closed));
            assert_eq!(pane(&middle, survivor).opacity, 0.0);
            assert_ne!(pane(&middle, survivor).rect, pane(&start, survivor).rect);
            let end = update(&mut state, &tab, false, now + ZOOM_DURATION);
            assert!(!state.is_running());
            assert_eq!(pane(&middle, survivor).layout, pane(&end, survivor).layout);
            assert_eq!(end.len(), tab.pane_ids().len());
            assert!(
                end.iter()
                    .all(|pane| pane.rect == pane.layout && pane.opacity == 1.0)
            );
        }
    }

    #[test]
    fn closing_a_zoomed_pane_animates_restoring_its_neighbors() {
        for (nested, extra) in [(false, false), (true, false), (true, true)] {
            let now = Instant::now();
            let mut tab = quadrants();
            if extra {
                tab.split(11, 12, Direction::Down, PathBuf::from("/tmp"));
            }
            tab.focus(2);
            if nested {
                tab.toggle_quadrant_zoom();
            }
            tab.toggle_zoom();
            let mut state = State::default();
            update(&mut state, &tab, false, now);
            assert!(tab.close(2));
            update(&mut state, &tab, false, now);
            assert!(state.is_running());
            let middle = update(&mut state, &tab, false, now + ZOOM_DURATION / 2);
            assert!(
                middle
                    .iter()
                    .all(|pane| pane.id != 2 && pane.opacity == 0.0)
            );
            let end = update(&mut state, &tab, false, now + ZOOM_DURATION);
            assert!(!state.is_running());
            assert_eq!(end.len(), tab.visible_panes(false).len());
            assert!(
                end.iter()
                    .all(|pane| pane.rect == pane.layout && pane.opacity == 1.0)
            );
        }
    }

    #[test]
    fn closing_during_quad_zoom_keeps_surviving_panes_in_place() {
        let now = Instant::now();
        let mut tab = quadrants();
        let mut state = State::default();
        update(&mut state, &tab, false, now);
        tab.toggle_quadrant_zoom();
        update(&mut state, &tab, false, now);
        let interrupted = now + Duration::from_millis(70);
        let before = update(&mut state, &tab, false, interrupted);
        assert!(tab.close(9));
        let after = update(&mut state, &tab, false, interrupted);
        assert!(state.is_running());
        assert!(after.iter().all(|pane| pane.id != 9));
        for old in before.iter().filter(|pane| pane.id != 9) {
            let next = pane(&after, old.id);
            assert_eq!(next.rect, old.rect);
            assert_eq!(next.layout, old.layout);
            assert_eq!(next.opacity, old.opacity);
            assert_eq!(next.background_opacity, old.background_opacity);
        }
        let end = update(&mut state, &tab, false, interrupted + ZOOM_DURATION);
        assert!(!state.is_running());
        assert_eq!(end.len(), tab.pane_ids().len());
    }

    #[test]
    fn splitting_or_resizing_out_of_zoom_animates_the_new_layout() {
        for split in [false, true] {
            let now = Instant::now();
            let mut tab = quadrants();
            tab.focus(2);
            tab.toggle_zoom();
            let mut state = State::default();
            let source = update(&mut state, &tab, false, now);
            if split {
                tab.split(11, 12, Direction::Down, PathBuf::from("/tmp"));
            } else {
                assert!(
                    tab.root
                        .resize(tab.focused, Direction::Right, 20.0, CONTENT)
                );
                tab.zoom = None;
                tab.quadrant_zoom = None;
            }
            assert_eq!(tab.zoom, None);
            let start = update(&mut state, &tab, false, now);
            assert!(state.is_running());
            assert_eq!(pane(&start, 2).rect, pane(&source, 2).rect);
            let end = update(&mut state, &tab, false, now + ZOOM_DURATION);
            assert!(!state.is_running());
            assert_eq!(end.len(), tab.pane_ids().len());
            assert_ne!(pane(&start, 2).layout, pane(&end, 2).layout);
        }
    }

    #[test]
    fn changed_minimum_cancels_zoom_and_keeps_cells_visible() {
        let now = Instant::now();
        let mut tab = quadrants();
        let mut state = State::default();
        update(&mut state, &tab, false, now);
        tab.toggle_zoom();
        update(&mut state, &tab, false, now);
        assert!(state.is_running());
        tab.zoom = None;
        let minimum = [CONTENT.width / 4.0, CONTENT.height / 2.0];
        let panes = state.update(
            &tab,
            false,
            CONTENT,
            16.0,
            minimum,
            1.0,
            &Config::default(),
            true,
            now,
        );
        assert!(!state.is_running());
        assert_eq!(panes.len(), 5);
        assert!(
            panes
                .iter()
                .all(|pane| pane.layout.width() + 2.0 >= minimum[0]
                    && pane.layout.height() + 2.0 >= minimum[1])
        );
    }

    #[test]
    fn changed_geometry_disabled_animation_and_finish_cancel_running_state() {
        let now = Instant::now();
        for changed in 0..6 {
            let mut tab = quadrants();
            let mut state = State::default();
            update(&mut state, &tab, false, now);
            tab.toggle_zoom();
            update(&mut state, &tab, false, now);
            let mut content = CONTENT;
            let mut scale = 1.0;
            let mut enabled = true;
            match changed {
                0 => enabled = false,
                1 => content.width += 1.0,
                2 => scale = 2.0,
                3 => tab.id += 1,
                4 => tab.root.equalize(),
                _ => state.finish(),
            }
            if changed == 4 {
                tab.root.split(5, 11, 12, Direction::Down);
            }
            let panes = state.update(
                &tab,
                false,
                content,
                16.0,
                [0.0; 2],
                scale,
                &Config::default(),
                enabled,
                now,
            );
            assert!(!state.is_running());
            assert!(
                panes
                    .iter()
                    .all(|pane| pane.rect == pane.layout && pane.opacity == 1.0)
            );
        }
    }
}
