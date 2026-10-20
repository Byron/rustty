//! Native GUI host state is aggregated before application notifications are sent.
use super::*;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scheme {
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    focused: bool,
    visible: bool,
    color_scheme: Option<Scheme>,
}
#[derive(Default)]
pub(super) struct ViewStates {
    states: BTreeMap<(u64, Id), (State, u64)>,
    order: u64,
}
impl ViewStates {
    pub(super) fn has_view(&self, client: u64, pane: Id) -> bool {
        self.states.contains_key(&(client, pane))
    }
    pub(super) fn focused(&self, client: u64, pane: Id) -> bool {
        self.states
            .get(&(client, pane))
            .is_some_and(|(state, _)| state.focused && state.visible)
    }
    fn aggregate(&self, pane: Id) -> State {
        let views = self.states.iter().filter(|((_, id), _)| *id == pane);
        let focused = views
            .clone()
            .any(|(_, (state, _))| state.focused && state.visible);
        let visible = views.clone().any(|(_, (state, _))| state.visible);
        let color_scheme = views
            .filter(|(_, (state, _))| state.color_scheme.is_some())
            .max_by_key(|(_, (state, order))| (state.focused && state.visible, *order))
            .and_then(|(_, (state, _))| state.color_scheme);
        State {
            focused,
            visible,
            color_scheme,
        }
    }
}
impl Host {
    pub(super) fn native_input(
        &mut self,
        client: u64,
        command: &tmuxx_command::Command,
        input: Option<Vec<u8>>,
    ) -> Result<Outcome, String> {
        if !self
            .clients
            .get(&client)
            .is_some_and(|client| client.context.host_effects)
        {
            return Err("native input requires a GUI connection".into());
        }
        let args = tmuxx_command::Args::parse(&command.args[1..], "t:")?;
        args.no_rest()?;
        let pane = args
            .get(b't')
            .and_then(|target| target.strip_prefix(b"%"))
            .and_then(|target| std::str::from_utf8(target).ok())
            .and_then(|target| target.parse::<Id>().ok())
            .ok_or("native input requires -t %PANE")?;
        let input = input.ok_or("missing native input")?;
        if input.len() > 8 * 1024 * 1024 {
            return Err("native input exceeds eight MiB".into());
        }
        let input: tmuxx_terminal::Input =
            serde_json::from_slice(&input).map_err(|error| error.to_string())?;
        let mut out = self.engine.native_input(pane, &input)?;
        if let Some(pane) = self.engine.panes.get(&pane) {
            self.client_became_latest(client, pane.window);
        }
        if let Some(session) = self.clients[&client]
            .context
            .session
            .and_then(|id| self.engine.sessions.get_mut(&id))
        {
            session.activity = self.engine.now;
        }
        self.effects(std::mem::take(&mut out.effects))?;
        Ok(out)
    }
    pub(super) fn view_state(
        &mut self,
        client: u64,
        command: &tmuxx_command::Command,
        input: Option<Vec<u8>>,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        if !self
            .clients
            .get(&client)
            .is_some_and(|client| client.context.host_effects)
        {
            return Err("host state requires a GUI connection".into());
        }
        let args = tmuxx_command::Args::parse(&command.args[1..], "t:")?;
        args.no_rest()?;
        let pane = args
            .get(b't')
            .and_then(|target| target.strip_prefix(b"%"))
            .and_then(|target| std::str::from_utf8(target).ok())
            .and_then(|target| target.parse::<Id>().ok())
            .ok_or("host state requires -t %PANE")?;
        if !self.engine.panes.contains_key(&pane) {
            return Err("pane not found".into());
        }
        let input = input.ok_or("missing host state")?;
        if input.len() > 512 {
            return Err("host state exceeds 512 bytes".into());
        }
        let state: State = serde_json::from_slice(&input).map_err(|error| error.to_string())?;
        if state.focused && state.visible {
            self.client_became_latest(client, self.engine.panes[&pane].window);
        }
        self.view_states
            .states
            .retain(|(_, pane), _| self.engine.panes.contains_key(pane));
        if self.view_states.states.len() >= 16_384
            && !self.view_states.states.contains_key(&(client, pane))
        {
            return Err("too many GUI view states".into());
        }
        self.view_states.order = self.view_states.order.wrapping_add(1);
        self.view_states
            .states
            .insert((client, pane), (state, self.view_states.order));
        if let Some(dark) = state.color_scheme {
            context.identity.insert(
                "client_theme".into(),
                match dark {
                    Scheme::Dark => "dark",
                    Scheme::Light => "light",
                }
                .into(),
            );
        }
        let generation = self.engine.panes[&pane].terminal.generation();
        self.apply_view_state(pane)?;
        Ok(Outcome {
            changed: self.engine.panes[&pane].terminal.generation() != generation,
            ..Outcome::default()
        })
    }
    fn apply_view_state(&mut self, pane: Id) -> Result<(), String> {
        let state = self.view_states.aggregate(pane);
        let focus_events = self
            .engine
            .server_options
            .get("focus-events")
            .is_some_and(|value| value == "on");
        let effects = self.engine.set_pane_host_state(
            pane,
            state.focused,
            state.visible,
            state
                .color_scheme
                .map(|scheme| matches!(scheme, Scheme::Dark)),
            focus_events,
        );
        self.effects(effects)
    }
    pub(super) fn refresh_view_states(&mut self) -> Result<(), String> {
        self.view_states
            .states
            .retain(|(_, pane), _| self.engine.panes.contains_key(pane));
        let panes = self
            .view_states
            .states
            .keys()
            .map(|(_, pane)| *pane)
            .collect::<std::collections::BTreeSet<_>>();
        for pane in panes {
            self.apply_view_state(pane)?;
        }
        Ok(())
    }
    pub(super) fn release_view_state(&mut self, client: u64, pane: Id) {
        if self.view_states.states.remove(&(client, pane)).is_some()
            && let Err(error) = self.apply_view_state(pane)
        {
            self.input_failed(pane, error);
        }
    }
    pub(super) fn disconnect_view_states(&mut self, client: u64) {
        let panes = self
            .view_states
            .states
            .keys()
            .filter_map(|(id, pane)| (*id == client).then_some(*pane))
            .collect::<Vec<_>>();
        for pane in panes {
            self.release_view_state(client, pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_views_aggregate_focus_and_prefer_focused_theme() {
        let mut states = ViewStates::default();
        states.states.insert(
            (1, 7),
            (
                State {
                    focused: true,
                    visible: true,
                    color_scheme: Some(Scheme::Dark),
                },
                1,
            ),
        );
        states.states.insert(
            (2, 7),
            (
                State {
                    focused: false,
                    visible: true,
                    color_scheme: Some(Scheme::Light),
                },
                2,
            ),
        );
        let state = states.aggregate(7);
        assert!(state.focused && state.visible);
        assert!(matches!(state.color_scheme, Some(Scheme::Dark)));
        states.states.remove(&(1, 7));
        let state = states.aggregate(7);
        assert!(!state.focused && state.visible);
        assert!(matches!(state.color_scheme, Some(Scheme::Light)));
        states.states.remove(&(2, 7));
        let state = states.aggregate(7);
        assert!(!state.focused && !state.visible);
    }
}
