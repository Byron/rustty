//! Synchronous native feedback from a published, payload-free owner view.
use egui::{Pos2, Rect};
use rustty::vt::dnd;
use rustty_app::platform::{DragEvent, DragResult};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_OFFER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Geometry {
    pub bounds: Rect,
    pub origin: Pos2,
    pub padding: [f32; 2],
    pub cell: [f32; 2],
    pub scale: f32,
    pub dimensions: [u16; 2],
}

impl Geometry {
    fn event(&self, position: [f32; 2], operations: dnd::Operations) -> Option<dnd::MoveEvent> {
        if !position.iter().all(|value| value.is_finite())
            || !self.bounds.contains(Pos2::from(position))
            || self.dimensions.contains(&0)
            || !self
                .cell
                .iter()
                .all(|value| value.is_finite() && *value > 0.0)
            || !self.scale.is_finite()
            || self.scale <= 0.0
        {
            return None;
        }
        let x = ((position[0] - self.origin.x) * self.scale - self.padding[0]).max(0.0);
        let y = ((position[1] - self.origin.y) * self.scale - self.padding[1]).max(0.0);
        Some(dnd::MoveEvent {
            cell_x: (x / self.cell[0])
                .floor()
                .min(f32::from(self.dimensions[0] - 1)) as u32,
            cell_y: (y / self.cell[1])
                .floor()
                .min(f32::from(self.dimensions[1] - 1)) as u32,
            pixel_x: x as i32,
            pixel_y: y as i32,
            operations: dnd::Operations {
                copy: operations.copy,
                move_: false,
            },
        })
    }
}

#[derive(Clone)]
pub(super) struct Target {
    pub pane: u64,
    pub identity: Option<u64>,
    pub geometry: Geometry,
    pub enabled: bool,
    pub view: Option<Arc<dnd::View>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Stamp {
    pub pane: u64,
    pub identity: u64,
    pub registration: u64,
    pub geometry: Geometry,
}

impl Target {
    fn stamp(&self) -> Option<Stamp> {
        Some(Stamp {
            pane: self.pane,
            identity: self.identity?,
            registration: self.view.as_ref()?.registration,
            geometry: self.geometry.clone(),
        })
    }
}

#[derive(Debug)]
pub(super) struct Dispatch {
    pub target: Stamp,
    pub command: dnd::Command,
}

struct Claim {
    target: Stamp,
    offer: u64,
    dropped: bool,
    last_event: Option<dnd::MoveEvent>,
    acceptance_after: u64,
}

#[derive(Default)]
pub(super) struct Controller {
    targets: Vec<Target>,
    blocked: Option<Arc<dyn Fn(Pos2) -> bool + Send + Sync>>,
    serial: Option<u64>,
    protocol: bool,
    mimes: Vec<Vec<u8>>,
    active: Option<Claim>,
    registration_changed: bool,
}

impl Controller {
    pub fn validates(&self, dispatch: &Dispatch) -> bool {
        self.targets
            .iter()
            .find(|target| target.pane == dispatch.target.pane)
            .is_some_and(|target| {
                target.identity == Some(dispatch.target.identity)
                    && target
                        .view
                        .as_ref()
                        .is_some_and(|view| view.registration == dispatch.target.registration)
                    && (matches!(dispatch.command.action, dnd::Action::Leave)
                        || target.enabled
                            && target.geometry == dispatch.target.geometry
                            && target.view.as_ref().is_some_and(|view| view.ready))
            })
    }

    pub fn publish(
        &mut self,
        mut targets: Vec<Target>,
        blocked: Option<Arc<dyn Fn(Pos2) -> bool + Send + Sync>>,
    ) -> Vec<Dispatch> {
        for target in &mut targets {
            // A disconnected registered target cannot become ordinary paste merely
            // because its owner view is temporarily unavailable.
            if target.identity.is_none() && target.view.is_none() {
                target.view = self
                    .targets
                    .iter()
                    .find(|old| old.pane == target.pane)
                    .and_then(|old| old.view.clone());
            }
        }
        let registration = |target: &Target| {
            target
                .view
                .as_ref()
                .map(|view| (target.pane, view.registration, view.ready))
        };
        self.registration_changed |= self
            .targets
            .iter()
            .filter_map(registration)
            .ne(targets.iter().filter_map(registration));
        self.targets = targets;
        self.blocked = blocked;
        let invalid = self.active.as_ref().is_some_and(|claim| {
            self.targets
                .iter()
                .find(|target| target.pane == claim.target.pane)
                .is_none_or(|target| {
                    !target.enabled
                        || target.stamp().as_ref() != Some(&claim.target)
                        || target.view.as_ref().is_none_or(|view| !view.ready)
                })
        });
        if invalid { self.leave() } else { Vec::new() }
    }

    pub fn registrations(&self) -> Vec<Vec<u8>> {
        let mut result: Vec<_> = self
            .targets
            .iter()
            .filter_map(|target| target.view.as_ref())
            .filter(|view| view.ready)
            .flat_map(|view| view.registered_mimes.split(|&byte| byte == b' '))
            .filter(|mime| dnd::valid_host_mime(mime))
            .collect();
        result.sort_unstable();
        result.dedup();
        result.into_iter().map(<[u8]>::to_vec).collect()
    }

    pub fn take_registration_change(&mut self) -> bool {
        std::mem::take(&mut self.registration_changed)
    }

    fn leave(&mut self) -> Vec<Dispatch> {
        self.active
            .take()
            .filter(|claim| !claim.dropped)
            .map(|claim| Dispatch {
                command: dnd::Command {
                    registration: claim.target.registration,
                    offer: claim.offer,
                    action: dnd::Action::Leave,
                },
                target: claim.target,
            })
            .into_iter()
            .collect()
    }

    pub fn failed(&mut self, offer: u64) {
        if self
            .active
            .as_ref()
            .is_some_and(|claim| claim.offer == offer)
        {
            self.active = None;
        }
    }

    pub fn handle(&mut self, event: DragEvent) -> (DragResult, Vec<Dispatch>) {
        let (serial, position, operations, items) = match event {
            DragEvent::Enter {
                serial,
                position,
                source_ops,
                mimes,
            } => {
                let leave = self.leave();
                self.serial = Some(serial);
                self.protocol = false;
                self.mimes.clear();
                for mime in mimes.into_iter().filter(|mime| dnd::valid_host_mime(mime)) {
                    if self.mimes.len() == dnd::MAX_ITEMS {
                        break;
                    }
                    if !self.mimes.contains(&mime) {
                        self.mimes.push(mime);
                    }
                }
                let (reply, mut commands) = self.update(serial, position, source_ops, None);
                let mut result = leave;
                result.append(&mut commands);
                return (reply, result);
            }
            DragEvent::Update {
                serial,
                position,
                source_ops,
            } => (serial, position, source_ops, None),
            DragEvent::Drop {
                serial,
                position,
                source_ops,
                items,
            } => (serial, position, source_ops, Some(items)),
            DragEvent::Leave { serial } => {
                if self.serial != Some(serial) {
                    return (DragResult::Reject, Vec::new());
                }
                let commands = self.leave();
                self.serial = None;
                self.mimes.clear();
                let reply = if self.protocol {
                    DragResult::Reject
                } else {
                    DragResult::Default
                };
                self.protocol = false;
                return (reply, commands);
            }
        };
        self.update(serial, position, operations, items)
    }

    fn update(
        &mut self,
        serial: u64,
        position: [f32; 2],
        operations: dnd::Operations,
        items: Option<Vec<dnd::Item>>,
    ) -> (DragResult, Vec<Dispatch>) {
        if self.serial != Some(serial) {
            return (DragResult::Reject, Vec::new());
        }
        let mut commands = Vec::new();
        let position_blocked = self
            .blocked
            .as_ref()
            .is_some_and(|blocked| blocked(Pos2::from(position)));
        let target = self
            .targets
            .iter()
            .rev()
            .find(|target| target.geometry.bounds.contains(Pos2::from(position)))
            .cloned();
        let Some(target) = target else {
            commands.extend(self.leave());
            return (
                if self.protocol {
                    DragResult::Reject
                } else {
                    DragResult::Default
                },
                commands,
            );
        };
        if target.view.is_some() {
            self.protocol = true;
        }
        // Once claimed, stale native drops must never turn into a fallback paste.
        let fallback = if self.protocol {
            DragResult::Reject
        } else {
            DragResult::Default
        };
        let Some(view) = target.view.as_ref() else {
            commands.extend(self.leave());
            return (fallback, commands);
        };
        let Some(stamp) = target
            .stamp()
            .filter(|_| target.enabled && view.ready && !position_blocked)
        else {
            commands.extend(self.leave());
            return (DragResult::Reject, commands);
        };
        let Some(event) = target.geometry.event(position, operations) else {
            return (DragResult::Reject, commands);
        };
        if self
            .active
            .as_ref()
            .is_none_or(|claim| claim.target != stamp)
        {
            commands.extend(self.leave());
            if items.is_some() {
                return (DragResult::Reject, commands);
            }
            let offer = NEXT_OFFER
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("drag offers exhausted");
            self.active = Some(Claim {
                target: stamp.clone(),
                offer,
                dropped: false,
                last_event: None,
                acceptance_after: view.revision,
            });
        }
        let claim = self.active.as_mut().unwrap();
        if claim.dropped {
            return (DragResult::Reject, commands);
        }
        if claim
            .last_event
            .is_some_and(|previous| previous.operations != event.operations)
        {
            claim.acceptance_after = view.revision;
        }
        let accepted = operations.copy
            && view.offer == Some(claim.offer)
            && view.revision > claim.acceptance_after
            && view.accepted == Some(dnd::Operation::Copy)
            && !view.dropped
            && self.mimes.iter().any(|mime| view.accepts_mime(mime));
        let feedback = if accepted {
            DragResult::Copy(
                self.mimes
                    .iter()
                    .filter(|mime| view.accepts_mime(mime))
                    .cloned()
                    .collect(),
            )
        } else {
            DragResult::Reject
        };
        let action = if let Some(mut items) = items {
            if !accepted {
                return (DragResult::Reject, commands);
            }
            items.retain(|item| self.mimes.contains(&item.mime) && view.accepts_mime(&item.mime));
            if !view.accepted_mimes.is_empty() {
                items.sort_by_key(|item| {
                    view.accepted_mimes
                        .split(|&byte| byte == 0)
                        .position(|mime| mime == item.mime)
                        .unwrap_or(usize::MAX)
                });
            }
            if items.is_empty()
                || items.len() > dnd::MAX_ITEMS
                || items
                    .iter()
                    .enumerate()
                    .any(|(index, item)| items[..index].iter().any(|old| old.mime == item.mime))
                || items
                    .iter()
                    .try_fold(0usize, |bytes, item| bytes.checked_add(item.data.len()))
                    .is_none_or(|bytes| bytes > dnd::MAX_DROP_BYTES)
            {
                return (DragResult::Reject, commands);
            }
            claim.dropped = true;
            dnd::Action::Drop { event, items }
        } else {
            if claim.last_event == Some(event) {
                return (feedback, commands);
            }
            claim.last_event = Some(event);
            dnd::Action::Move {
                event,
                mimes: self.mimes.clone(),
            }
        };
        commands.push(Dispatch {
            target: stamp,
            command: dnd::Command {
                registration: view.registration,
                offer: claim.offer,
                action,
            },
        });
        (feedback, commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pane: u64, left: f32, registered: bool) -> Target {
        Target {
            pane,
            identity: Some(100 + pane),
            geometry: Geometry {
                bounds: Rect::from_min_size(Pos2::new(left, 0.0), egui::vec2(100.0, 80.0)),
                origin: Pos2::new(left, 0.0),
                padding: [4.0, 6.0],
                cell: [10.0, 20.0],
                scale: 2.0,
                dimensions: [20, 8],
            },
            enabled: true,
            view: registered.then(|| {
                Arc::new(dnd::View {
                    registration: 10 + pane,
                    revision: 1,
                    ready: true,
                    registered_mimes: b"text/plain image/png".to_vec(),
                    ..Default::default()
                })
            }),
        }
    }
    fn enter(serial: u64, x: f32) -> DragEvent {
        DragEvent::Enter {
            serial,
            position: [x, 20.0],
            source_ops: dnd::Operations {
                copy: true,
                move_: true,
            },
            mimes: vec![b"text/plain".to_vec(), b"image/png".to_vec()],
        }
    }
    fn update(serial: u64, x: f32) -> DragEvent {
        DragEvent::Update {
            serial,
            position: [x, 20.0],
            source_ops: dnd::Operations {
                copy: true,
                move_: true,
            },
        }
    }
    fn accept(controller: &mut Controller, mut target: Target, offer: u64) -> Target {
        let view = Arc::make_mut(target.view.as_mut().unwrap());
        view.offer = Some(offer);
        view.revision += 1;
        view.accepted = Some(dnd::Operation::Copy);
        controller.publish(vec![target.clone()], None);
        target
    }

    #[test]
    fn default_pending_and_empty_registrations_have_distinct_feedback() {
        let mut controller = Controller::default();
        controller.publish(vec![target(1, 0.0, false)], None);
        assert_eq!(controller.handle(enter(1, 20.0)).0, DragResult::Default);
        let mut pending = target(1, 0.0, true);
        Arc::make_mut(pending.view.as_mut().unwrap()).ready = false;
        Arc::make_mut(pending.view.as_mut().unwrap())
            .registered_mimes
            .clear();
        controller.publish(vec![pending.clone()], None);
        let (reply, commands) = controller.handle(update(1, 20.0));
        assert_eq!(reply, DragResult::Reject);
        assert!(commands.is_empty());
        Arc::make_mut(pending.view.as_mut().unwrap()).ready = true;
        controller.publish(vec![pending], None);
        let (reply, commands) = controller.handle(update(1, 20.0));
        assert_eq!(reply, DragResult::Reject);
        assert_eq!(commands.len(), 1);
        assert!(controller.registrations().is_empty());
    }

    #[test]
    fn only_current_completed_offer_acceptance_can_capture_one_filtered_drop() {
        let mut controller = Controller::default();
        let target = target(1, 0.0, true);
        controller.publish(vec![target.clone()], None);
        let (reply, commands) = controller.handle(enter(1, 20.0));
        assert_eq!(reply, DragResult::Reject);
        let offer = commands[0].command.offer;
        let dnd::Action::Move { event, .. } = commands[0].command.action else {
            panic!()
        };
        assert_eq!([event.cell_x, event.cell_y], [3, 1]);
        assert_eq!([event.pixel_x, event.pixel_y], [36, 34]);
        assert_eq!(
            event.operations,
            dnd::Operations {
                copy: true,
                move_: false
            }
        );
        let mut accepted = accept(&mut controller, target, offer + 1);
        assert_eq!(controller.handle(update(1, 20.0)).0, DragResult::Reject);
        let view = Arc::make_mut(accepted.view.as_mut().unwrap());
        view.offer = Some(offer);
        view.accepted_mimes = b"image/png\0".to_vec();
        controller.publish(vec![accepted], None);
        let (reply, commands) = controller.handle(update(1, 20.0));
        assert_eq!(reply, DragResult::Copy(vec![b"image/png".to_vec()]));
        assert!(
            commands.is_empty(),
            "stationary feedback does not repeatedly write OSC bytes"
        );
        let drop_event = DragEvent::Drop {
            serial: 1,
            position: [20.0, 20.0],
            source_ops: dnd::Operations {
                copy: true,
                move_: false,
            },
            items: vec![
                dnd::Item {
                    mime: b"text/plain".to_vec(),
                    data: b"unused".to_vec(),
                },
                dnd::Item {
                    mime: b"image/png".to_vec(),
                    data: vec![1, 2, 3],
                },
            ],
        };
        let (reply, commands) = controller.handle(drop_event.clone());
        assert_eq!(reply, DragResult::Copy(vec![b"image/png".to_vec()]));
        let dnd::Action::Drop { items, .. } = &commands[0].command.action else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].data, [1, 2, 3]);
        assert_eq!(controller.handle(drop_event).0, DragResult::Reject);
        assert!(
            controller
                .handle(DragEvent::Leave { serial: 1 })
                .1
                .is_empty(),
            "leave cannot discard owner-held drop data"
        );
        assert!(
            controller.validates(&commands[0]),
            "a queued accepted drop survives native leave"
        );
    }

    #[test]
    fn pane_changes_leave_then_reoffer_and_stale_targets_never_become_paste() {
        let mut controller = Controller::default();
        let first = target(1, 0.0, true);
        let second = target(2, 100.0, true);
        controller.publish(vec![first.clone(), second.clone()], None);
        let first_offer = controller.handle(enter(1, 20.0)).1[0].command.offer;
        let (_, commands) = controller.handle(update(1, 120.0));
        assert_eq!(commands.len(), 2);
        assert!(matches!(commands[0].command.action, dnd::Action::Leave));
        assert_eq!(commands[0].target.pane, 1);
        assert_eq!(commands[0].command.offer, first_offer);
        assert_eq!(commands[1].target.pane, 2);
        assert_ne!(commands[1].command.offer, first_offer);
        let mut replacement = second.clone();
        replacement.identity = Some(999);
        let invalidated = controller.publish(vec![first, replacement], None);
        assert_eq!(invalidated.len(), 1);
        assert!(!controller.validates(&commands[1]));
        controller.publish(vec![target(2, 100.0, false)], None);
        assert_eq!(controller.handle(update(1, 120.0)).0, DragResult::Reject);
        assert_eq!(controller.handle(enter(2, 120.0)).0, DragResult::Default);
        assert_eq!(controller.handle(update(1, 120.0)).0, DragResult::Reject);
    }

    #[test]
    fn geometry_overlay_and_disconnect_invalidate_registered_claims() {
        let mut controller = Controller::default();
        let first = target(1, 0.0, true);
        controller.publish(vec![first.clone()], None);
        let offer = controller.handle(enter(1, 20.0)).1[0].command.offer;
        let mut changed = accept(&mut controller, first, offer);
        changed.geometry.scale = 1.0;
        assert_eq!(controller.publish(vec![changed.clone()], None).len(), 1);
        assert_eq!(controller.handle(update(1, 20.0)).0, DragResult::Reject);
        controller.publish(vec![changed.clone()], Some(Arc::new(|_| true)));
        assert_eq!(controller.handle(update(1, 20.0)).0, DragResult::Reject);
        changed.identity = None;
        changed.view = None;
        controller.publish(vec![changed], None);
        assert!(!controller.registrations().is_empty());
        assert_eq!(controller.handle(enter(2, 20.0)).0, DragResult::Reject);
    }

    #[test]
    fn source_operation_changes_require_fresh_copy_acceptance() {
        let mut controller = Controller::default();
        let first = target(1, 0.0, true);
        controller.publish(vec![first.clone()], None);
        let offer = controller.handle(enter(1, 20.0)).1[0].command.offer;
        let mut accepted = accept(&mut controller, first, offer);
        let move_only = DragEvent::Update {
            serial: 1,
            position: [20.0, 20.0],
            source_ops: dnd::Operations {
                copy: false,
                move_: true,
            },
        };
        assert_eq!(controller.handle(move_only).0, DragResult::Reject);
        assert_eq!(controller.handle(update(1, 20.0)).0, DragResult::Reject);
        Arc::make_mut(accepted.view.as_mut().unwrap()).revision += 1;
        controller.publish(vec![accepted], None);
        assert_eq!(
            controller.handle(update(1, 20.0)).0,
            DragResult::Copy(vec![b"text/plain".to_vec(), b"image/png".to_vec()])
        );
    }

    #[test]
    fn feedback_contains_only_offered_accepted_types_in_source_order() {
        let mut controller = Controller::default();
        let first = target(1, 0.0, true);
        controller.publish(vec![first.clone()], None);
        let offer = controller.handle(enter(1, 20.0)).1[0].command.offer;
        let mut accepted = accept(&mut controller, first, offer);
        Arc::make_mut(accepted.view.as_mut().unwrap()).accepted_mimes =
            b"image/png\0application/x-missing\0text/plain\0".to_vec();
        controller.publish(vec![accepted.clone()], None);
        assert_eq!(
            controller.handle(update(1, 20.0)).0,
            DragResult::Copy(vec![b"text/plain".to_vec(), b"image/png".to_vec()])
        );
        Arc::make_mut(accepted.view.as_mut().unwrap()).accepted_mimes =
            b"application/x-missing\0".to_vec();
        controller.publish(vec![accepted], None);
        assert_eq!(controller.handle(update(1, 20.0)).0, DragResult::Reject);
    }

    #[test]
    fn clipped_hit_regions_keep_the_original_cell_origin_and_invalid_geometry_rejects() {
        let mut geometry = target(1, -20.0, true).geometry;
        geometry.bounds = geometry.bounds.intersect(Rect::from_min_max(
            Pos2::new(0.0, 0.0),
            Pos2::new(100.0, 80.0),
        ));
        let event = geometry
            .event([5.0, 20.0], dnd::Operations::default())
            .unwrap();
        assert_eq!([event.pixel_x, event.pixel_y], [46, 34]);
        assert_eq!([event.cell_x, event.cell_y], [4, 1]);
        assert!(
            geometry
                .event([-1.0, 20.0], dnd::Operations::default())
                .is_none()
        );
        assert!(
            geometry
                .event([f32::NAN, 20.0], dnd::Operations::default())
                .is_none()
        );
        geometry.cell[0] = 0.0;
        assert!(
            geometry
                .event([5.0, 20.0], dnd::Operations::default())
                .is_none()
        );
    }

    #[test]
    fn acceptance_publications_do_not_rescan_native_type_registrations() {
        let mut controller = Controller::default();
        let first = target(1, 0.0, true);
        controller.publish(vec![first.clone()], None);
        assert!(controller.take_registration_change());
        let offer = controller.handle(enter(1, 20.0)).1[0].command.offer;
        let mut accepted = accept(&mut controller, first, offer);
        assert!(!controller.take_registration_change());
        Arc::make_mut(accepted.view.as_mut().unwrap()).ready = false;
        controller.publish(vec![accepted], None);
        assert!(controller.take_registration_change());
        assert!(controller.registrations().is_empty());
    }

    #[test]
    fn topmost_disabled_overlay_never_routes_to_the_pane_below() {
        let mut controller = Controller::default();
        let mut overlay = target(2, 0.0, true);
        overlay.enabled = false;
        controller.publish(vec![target(1, 0.0, true), overlay.clone()], None);
        let (reply, commands) = controller.handle(enter(1, 20.0));
        assert_eq!(reply, DragResult::Reject);
        assert!(commands.is_empty());
        overlay.enabled = true;
        controller.publish(vec![target(1, 0.0, true), overlay], None);
        let (_, commands) = controller.handle(update(1, 20.0));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].target.pane, 2);
    }
}
