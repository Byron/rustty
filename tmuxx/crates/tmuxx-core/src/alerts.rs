use super::*;

impl Engine {
    pub(super) fn finish_window_selections(&mut self, current: Option<Id>) -> Vec<Effect> {
        let selected = self
            .sessions
            .values_mut()
            .flat_map(|session| std::mem::take(&mut session.selected_windows))
            .collect::<Vec<_>>();
        let mut alerted = BTreeSet::new();
        let mut effects = Vec::new();
        for wid in selected.into_iter().filter(|wid| alerted.insert(*wid)) {
            if !self.windows.contains_key(&wid) {
                continue;
            }
            for session in self.sessions.values_mut() {
                for (index, link) in &mut session.links {
                    if session.windows.get(index) == Some(&wid) {
                        link.alerts = 0;
                    }
                }
            }
            self.windows.get_mut(&wid).unwrap().activity = self.now;
            effects.extend(self.window_alerts(wid, 1, current));
        }
        effects
    }
    pub(super) fn reset_silence(&mut self, wid: Id) {
        let seconds = self
            .window_option(wid, "monitor-silence")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        self.windows.get_mut(&wid).unwrap().silence_deadline =
            (seconds != 0).then(|| self.now.saturating_add(seconds.saturating_mul(1_000_000)));
    }
    pub(super) fn reset_silence_all(&mut self) {
        for wid in self.windows.keys().copied().collect::<Vec<_>>() {
            self.reset_silence(wid);
        }
    }
    /// Poll from the owner clock. The pure engine never creates a timer thread.
    pub fn alert_tick(&mut self) -> Vec<Effect> {
        let windows = self
            .windows
            .values()
            .filter(|window| {
                window
                    .silence_deadline
                    .is_some_and(|deadline| deadline <= self.now)
            })
            .map(|window| window.id)
            .collect::<Vec<_>>();
        windows
            .into_iter()
            .flat_map(|wid| self.window_alerts(wid, 4, None))
            .collect()
    }
    pub(super) fn window_alerts(&mut self, wid: Id, bits: u8, current: Option<Id>) -> Vec<Effect> {
        self.reset_silence(wid);
        let mut effects = Vec::new();
        // C coalesces pending bits, then handles bell, activity and silence in this order.
        for (bit, monitor, action, event) in [
            (2, "monitor-bell", "bell-action", "alert-bell"),
            (1, "monitor-activity", "activity-action", "alert-activity"),
            (4, "monitor-silence", "silence-action", "alert-silence"),
        ] {
            if bits & bit == 0
                || self
                    .window_option(wid, monitor)
                    .is_none_or(|value| matches!(value, "off" | "0"))
            {
                continue;
            }
            let mut links = self
                .sessions
                .values()
                .flat_map(|session| {
                    session
                        .windows
                        .iter()
                        .filter(move |(_, window)| **window == wid)
                        .map(move |(index, _)| (session.links[index].order, session.id, *index))
                })
                .collect::<Vec<_>>();
            links.sort_unstable();
            for (_, sid, index) in links {
                let selected = self.sessions[&sid].active == index;
                let attached = current == Some(sid)
                    || self
                        .clients
                        .iter()
                        .any(|client| client.session == Some(sid));
                let applies = match self.session_option(sid, action).unwrap_or("none") {
                    "any" => true,
                    "current" => selected,
                    "other" => !selected,
                    _ => false,
                };
                let link = self
                    .sessions
                    .get_mut(&sid)
                    .unwrap()
                    .links
                    .get_mut(&index)
                    .unwrap();
                if bit != 2 && link.alerts & bit != 0 {
                    continue;
                }
                if !selected || !attached {
                    link.alerts |= bit;
                }
                if applies {
                    effects.push(Effect::Alert {
                        session: sid,
                        index,
                        window: wid,
                        event,
                    });
                }
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_pane_removal_flushes_selected_window_alerts_without_another_command() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        for args in [
            vec!["new-session", "-d", "-s", "main", "sleep 120"],
            vec!["new-window", "-d", "-t", "main:4", "sleep 120"],
            vec!["set", "-w", "-t", "main:4", "monitor-activity", "on"],
            vec!["set", "-t", "main", "activity-action", "any"],
        ] {
            let commands =
                tmuxx_command::parse_argv(args.iter().map(|arg| arg.as_bytes())).unwrap();
            engine.execute(&commands[0], &mut context).unwrap();
        }
        engine.feed(1, b"activity\x07");
        assert_eq!(engine.sessions[&0].links[&4].alerts, 3);
        for exited in [false, true] {
            let mut engine = engine.clone();
            let outcome = if exited {
                engine.exited(0, 0)
            } else {
                engine.close_pane(0)
            };
            assert_eq!(engine.sessions[&0].active, 4);
            assert_eq!(engine.sessions[&0].links[&4].alerts, 1);
            assert!(engine.sessions[&0].selected_windows.is_empty());
            assert!(outcome.effects.iter().any(|effect| matches!(
                effect,
                Effect::Alert {
                    event: "alert-activity",
                    window: 1,
                    ..
                }
            )));
        }
    }
}
