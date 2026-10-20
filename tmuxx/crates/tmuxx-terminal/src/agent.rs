//! Pane metadata retained independently of host actions and terminal display state.
use crate::Effect;
use rustty_vt::agent::{Event, Snapshot};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentStatus {
    pub snapshot: Option<Snapshot>,
    pub generation: u64,
    pub command_running: bool,
    pub enclosing_command: bool,
}

impl AgentStatus {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
        }
        if self.enclosing_command && (!self.command_running || self.snapshot.is_none()) {
            return Err("invalid retained agent command state");
        }
        Ok(())
    }

    /// Return only the implicit end of an agent's enclosing shell command.
    pub fn observe(&mut self, effect: &Effect) -> Option<Event> {
        match effect {
            Effect::AgentStatus(Event::Begin(snapshot)) => {
                self.generation = self.generation.wrapping_add(1);
                self.snapshot = Some(snapshot.clone());
                self.enclosing_command = self.command_running;
            }
            Effect::AgentStatus(Event::Update(snapshot)) if self.snapshot.is_some() => {
                self.snapshot = Some(snapshot.clone());
            }
            Effect::AgentStatus(Event::End) => {
                self.snapshot = None;
                self.enclosing_command = false;
            }
            Effect::CommandStart => self.command_running = true,
            Effect::CommandEnd { .. } => {
                self.command_running = false;
                if self.enclosing_command {
                    self.snapshot = None;
                    self.enclosing_command = false;
                    return Some(Event::End);
                }
            }
            _ => {}
        }
        None
    }

    /// Reconcile retained metadata without repeating an unchanged registration.
    pub fn event_since(&self, previous: &Self) -> Option<Event> {
        match (&self.snapshot, &previous.snapshot) {
            (Some(snapshot), old) if old.is_none() || self.generation != previous.generation => {
                Some(Event::Begin(snapshot.clone()))
            }
            (Some(snapshot), Some(old)) if snapshot != old => Some(Event::Update(snapshot.clone())),
            (None, Some(_)) => Some(Event::End),
            _ => None,
        }
    }
}
