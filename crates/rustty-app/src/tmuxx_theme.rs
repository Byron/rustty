use super::*;

impl App {
    pub(super) fn reload_tmuxx_themes(&mut self) {
        self.tmuxx_themes.clear();
        let names = self
            .config()
            .tmuxx_theme_hosts
            .values()
            .chain(self.config().tmuxx_theme_sessions.values())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let existing = self
            .loaded
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<HashSet<_>>();
        for name in names {
            let loaded = self.config_loader.load_with_theme(&self.config_args, &name);
            let errors = loaded
                .diagnostics
                .iter()
                .map(ToString::to_string)
                .filter(|error| !existing.contains(error))
                .collect::<Vec<_>>();
            if errors.is_empty() {
                self.tmuxx_themes.insert(
                    name,
                    theme_preview::appearance(self.config(), loaded.config),
                );
            } else {
                self.errors.extend(
                    errors
                        .into_iter()
                        .map(|error| format!("tmuxx theme {name}: {error}")),
                );
            }
        }
    }

    pub(super) fn tmuxx_theme(&self, window: Id) -> Option<&str> {
        let entry = self.tmuxx_window(window)?;
        let host = match &entry.source {
            tmuxx_host::Source::Ssh(host) | tmuxx_host::Source::SystemSsh(host) => {
                Some(host.as_str())
            }
            _ => None,
        };
        self.config()
            .tmuxx_theme_for(host, entry.mapping.session_names.get(&window)?)
    }

    pub(super) fn tmuxx_theme_scope(&self, window: Id) -> Option<theme_preview::Scope> {
        let entry = self.tmuxx_window(window)?;
        let host = match &entry.source {
            tmuxx_host::Source::Ssh(host) | tmuxx_host::Source::SystemSsh(host) => {
                Some(host.as_str())
            }
            _ => None,
        };
        theme_preview::Scope::for_connection(
            self.config(),
            host,
            entry.mapping.session_names.get(&window)?,
        )
    }

    pub(super) fn window_config(&self, window: Id) -> &Config {
        self.tmuxx_theme(window)
            .and_then(|name| self.tmuxx_themes.get(name))
            .unwrap_or_else(|| self.config())
    }

    pub(super) fn pane_window(&self, pane: Id) -> Option<Id> {
        self.workspace
            .windows
            .iter()
            .find(|window| window.tabs.iter().any(|tab| tab.panes.contains_key(&pane)))
            .map(|window| window.id)
    }

    pub(super) fn window_configs(&self) -> BTreeMap<Id, Config> {
        self.workspace
            .windows
            .iter()
            .map(|window| (window.id, self.window_config(window.id).clone()))
            .collect()
    }

    pub(super) fn apply_pane_configs(
        &mut self,
        apply: fn(&Session, &Config) -> std::io::Result<()>,
        scope: &theme_preview::Scope,
    ) {
        let configs = self.window_configs();
        for (&id, pane) in &self.panes {
            if *scope != theme_preview::Scope::Global
                && self
                    .pane_window(id)
                    .and_then(|window| self.tmuxx_theme_scope(window))
                    .as_ref()
                    != Some(scope)
            {
                continue;
            }
            let config = self
                .pane_window(id)
                .and_then(|window| configs.get(&window))
                .unwrap_or_else(|| self.config());
            if let Err(error) = apply(&pane.session, config) {
                self.errors.push(error.to_string());
            }
        }
    }

    pub(super) fn window_color_scheme(&self, window: Id) -> vt::query::ColorScheme {
        color_scheme(if self.tmuxx_theme(window).is_some() {
            window_ui_theme(self.window_config(window), self.config_loader.dark_mode)
                == egui::Theme::Dark
        } else {
            self.config_loader.dark_mode
        })
    }

    pub(super) fn sync_tmuxx_themes(&mut self) {
        let changed = self
            .windows
            .iter()
            .filter_map(|(native, host)| {
                let name = self.tmuxx_theme(host.id);
                (host.tmuxx_theme.as_deref() != name).then(|| (*native, name.map(str::to_owned)))
            })
            .collect::<Vec<_>>();
        for (native, name) in changed {
            let mut host = self.windows.remove(&native).unwrap();
            let config = self.window_config(host.id).clone();
            host.tmuxx_theme = name;
            if let Some(window) = self
                .workspace
                .windows
                .iter()
                .find(|window| window.id == host.id)
            {
                for id in window.tabs.iter().flat_map(Tab::pane_ids) {
                    if let Some(pane) = self.panes.get(&id)
                        && let Err(error) = pane.session.apply_appearance(&config)
                    {
                        self.errors.push(error.to_string());
                    }
                }
                if let Some(platform) = &self.platform
                    && let Err(error) =
                        platform.configure_window(&host.window, window.quick, &config)
                {
                    self.errors.push(error);
                }
            }
            match rustty_render::Renderer::new(font_config(
                &config,
                host.window.scale_factor() as f32,
            )) {
                Ok(fonts) => host.fonts = fonts,
                Err(error) => self.errors.push(error.to_string()),
            }
            host.repaint();
            self.windows.insert(native, host);
        }
    }
}

pub(super) fn window_ui_theme(config: &Config, system_dark: bool) -> egui::Theme {
    match ui_theme(config) {
        egui::ThemePreference::Dark => egui::Theme::Dark,
        egui::ThemePreference::Light => egui::Theme::Light,
        egui::ThemePreference::System => {
            if system_dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            }
        }
    }
}
