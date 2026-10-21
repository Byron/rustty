//! Captured native menu commands retain the server identity they were shown for.
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Embedded(PathBuf),
    Local(PathBuf),
    Ssh(String),
    SystemSsh(String),
}

impl Source {
    pub fn ssh_target(&self) -> Option<String> {
        match self {
            Self::Ssh(host) => Some(format!("ssh:{host}")),
            Self::SystemSsh(host) => Some(format!("system-ssh:{host}")),
            Self::Embedded(_) | Self::Local(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub source: Source,
    pub instance: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Connect(String),
    Open(Target, u64),
    Kill(Target, u64),
    Execute(Target, Vec<String>),
    Disconnect(Target),
    Reconnect(Target),
    Shutdown(Target),
    ShutdownIdle(Target),
}

impl Command {
    pub fn requires_confirmation(&self) -> bool {
        matches!(
            self,
            Self::Kill(..) | Self::Execute(..) | Self::Shutdown(..) | Self::ShutdownIdle(..)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub command: Option<Command>,
    pub children: Vec<Entry>,
}

impl Entry {
    pub fn command(label: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            command: Some(command),
            children: Vec::new(),
        }
    }
    pub fn label(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: None,
            children: Vec::new(),
        }
    }
    pub fn submenu(label: impl Into<String>, children: Vec<Entry>) -> Self {
        Self {
            label: label.into(),
            command: None,
            children,
        }
    }
}

pub fn actions() -> [(&'static str, rustty::config::Action); 5] {
    use rustty::config::Action;
    [
        ("New Temporary Window", Action::NewTemporaryTmuxxWindow),
        ("New Persistent Window", Action::NewPersistentTmuxxWindow),
        (
            "Copy Window to Persistent Session",
            Action::CopyWindowToTmuxx,
        ),
        ("Connect to Server…", Action::ConnectTmuxx),
        ("New Floating Panel", Action::NewFloatingPane),
    ]
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) mod native {
    use super::{Command, Entry};
    use muda::{IsMenuItem, MenuItem, PredefinedMenuItem, Submenu};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    pub(crate) struct Routes {
        next_id: u64,
        commands: HashMap<String, Command>,
    }
    impl Routes {
        fn insert(&mut self, command: Command) -> String {
            let id = format!("rustty-tmuxx-{}", self.next_id);
            self.next_id += 1;
            self.commands.insert(id.clone(), command);
            id
        }
        pub fn get(&self, id: &str) -> Option<Command> {
            self.commands.get(id).cloned()
        }
    }

    pub(crate) struct NativeMenu {
        pub menu: Submenu,
        pub routes: Arc<Mutex<Routes>>,
        entries: Vec<Entry>,
        items: Vec<Box<dyn IsMenuItem>>,
    }
    impl NativeMenu {
        pub fn new() -> Self {
            Self {
                menu: Submenu::new("tmuxx", true),
                routes: Arc::default(),
                entries: Vec::new(),
                items: Vec::new(),
            }
        }
        pub fn update(&mut self, entries: Vec<Entry>) -> Result<(), String> {
            if entries == self.entries {
                return Ok(());
            }
            let mut routes = Routes {
                next_id: self.routes.lock().map_err(|e| e.to_string())?.next_id,
                commands: HashMap::new(),
            };
            let mut items: Vec<Box<dyn IsMenuItem>> = Vec::new();
            if !entries.is_empty() {
                items.push(Box::new(PredefinedMenuItem::separator()));
            }
            for entry in &entries {
                items.push(item(entry, &mut routes)?);
            }
            for old in &self.items {
                self.menu.remove(old.as_ref()).map_err(|e| e.to_string())?;
            }
            for item in &items {
                self.menu.append(item.as_ref()).map_err(|e| e.to_string())?;
            }
            *self.routes.lock().map_err(|e| e.to_string())? = routes;
            self.entries = entries;
            self.items = items;
            Ok(())
        }
    }
    fn item(entry: &Entry, routes: &mut Routes) -> Result<Box<dyn IsMenuItem>, String> {
        #[cfg(target_os = "windows")]
        let label = entry.label.replace('&', "&&");
        #[cfg(not(target_os = "windows"))]
        let label = &entry.label;
        if !entry.children.is_empty() {
            let menu = Submenu::new(label, true);
            for child in &entry.children {
                menu.append(item(child, routes)?.as_ref())
                    .map_err(|e| e.to_string())?;
            }
            Ok(Box::new(menu))
        } else if let Some(command) = &entry.command {
            Ok(Box::new(MenuItem::with_id(
                routes.insert(command.clone()),
                label,
                true,
                None,
            )))
        } else {
            Ok(Box::new(MenuItem::new(label, false, None)))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::tmuxx_menu::{Source, Target};

        #[test]
        fn replacing_menu_contents_retires_old_ids_and_keeps_captured_targets() {
            let target = Target {
                source: Source::Ssh("build-host".into()),
                instance: "first".into(),
            };
            let command = Command::Kill(target, 7);
            let mut routes = Routes::default();
            let old = routes.insert(command.clone());
            assert_eq!(routes.get(&old), Some(command));
            routes.commands.clear();
            let new = routes.insert(Command::Open(
                Target {
                    source: Source::Ssh("build-host".into()),
                    instance: "second".into(),
                },
                7,
            ));
            assert_ne!(old, new);
            assert!(routes.get(&old).is_none());
            assert!(routes.get(&new).is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_native_menu_commands_require_confirmation() {
        let target = Target {
            source: Source::Local("/tmp/test.native".into()),
            instance: "daemon".into(),
        };
        assert!(Command::Kill(target.clone(), 7).requires_confirmation());
        assert!(Command::Shutdown(target.clone()).requires_confirmation());
        assert!(Command::ShutdownIdle(target.clone()).requires_confirmation());
        assert!(!Command::Reconnect(target.clone()).requires_confirmation());
        assert!(Command::Execute(target.clone(), vec!["kill-pane".into()]).requires_confirmation());
        assert!(!Command::Open(target.clone(), 7).requires_confirmation());
        assert!(!Command::Disconnect(target).requires_confirmation());
        assert!(!Command::Connect("ssh:build-host".into()).requires_confirmation());
    }
}
