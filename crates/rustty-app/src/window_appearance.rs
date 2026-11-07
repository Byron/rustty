use rustty::config::{Config, ConfigLoader, LoadedConfig, WindowTheme};
use winit::window::Theme;

/// The real OS preference stays separate from this session's application override.
pub(super) struct Appearance {
    pub system: Theme,
    pub override_: Option<WindowTheme>,
}

impl Appearance {
    pub fn new(system: Theme) -> Self {
        Self {
            system,
            override_: None,
        }
    }

    pub fn theme(&self, config: &Config) -> Theme {
        match self.override_.unwrap_or(config.window_theme) {
            WindowTheme::System => self.system,
            WindowTheme::Light => Theme::Light,
            WindowTheme::Dark => Theme::Dark,
            WindowTheme::Auto => {
                let color = config.background;
                if 299 * u32::from(color.r) + 587 * u32::from(color.g) + 114 * u32::from(color.b)
                    < 128000
                {
                    Theme::Dark
                } else {
                    Theme::Light
                }
            }
        }
    }

    pub fn ui_theme(&self, config: &Config) -> egui::Theme {
        match self.theme(config) {
            Theme::Light => egui::Theme::Light,
            Theme::Dark => egui::Theme::Dark,
        }
    }

    pub fn load(
        &self,
        loader: &ConfigLoader,
        args: &[String],
        theme: Option<&str>,
    ) -> LoadedConfig {
        let mut loader = loader.clone();
        loader.dark_mode = self.system == Theme::Dark;
        let load = |loader: &ConfigLoader| match theme {
            Some(theme) => loader.load_with_theme(args, theme),
            None => loader.load_with_args(args),
        };
        let loaded = load(&loader);
        let dark = self.theme(&loaded.config) == Theme::Dark;
        if loader.dark_mode == dark
            || loaded
                .config
                .theme
                .as_ref()
                .is_none_or(|theme| theme.light == theme.dark)
        {
            return loaded;
        }
        // Resolve conditional terminal palettes using the same scheme as native chrome.
        loader.dark_mode = dark;
        load(&loader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::Rgb;

    #[test]
    fn appearance_override_resolves_config_system_and_background_for_every_consumer() {
        for system in [Theme::Light, Theme::Dark] {
            for (configured, background, expected) in [
                (WindowTheme::System, Rgb::new(0, 0, 0), system),
                (WindowTheme::Light, Rgb::new(0, 0, 0), Theme::Light),
                (WindowTheme::Dark, Rgb::new(255, 255, 255), Theme::Dark),
                (WindowTheme::Auto, Rgb::new(127, 127, 127), Theme::Dark),
                (WindowTheme::Auto, Rgb::new(128, 128, 128), Theme::Light),
            ] {
                let mut config = Config::default();
                config.window_theme = configured;
                config.background = background;
                for (override_, expected) in [
                    (None, expected),
                    (Some(WindowTheme::System), system),
                    (Some(WindowTheme::Light), Theme::Light),
                    (Some(WindowTheme::Dark), Theme::Dark),
                ] {
                    let appearance = Appearance { system, override_ };
                    assert_eq!(appearance.theme(&config), expected);
                    assert_eq!(
                        appearance.ui_theme(&config),
                        if expected == Theme::Dark {
                            egui::Theme::Dark
                        } else {
                            egui::Theme::Light
                        }
                    );
                    assert_eq!(
                        super::super::color_scheme(appearance.theme(&config) == Theme::Dark),
                        if expected == Theme::Dark {
                            rustty::vt::query::ColorScheme::Dark
                        } else {
                            rustty::vt::query::ColorScheme::Light
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn appearance_reload_and_scoped_themes_keep_override_until_config_reset() {
        let directory = std::env::temp_dir().join(format!(
            "rustty-appearance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let own = directory.join("config/rustty");
        std::fs::create_dir_all(own.join("themes")).unwrap();
        let path = own.join("config.rustty");
        let original = "theme = light:Day,dark:Night\nwindow-theme = light\n";
        std::fs::write(&path, original).unwrap();
        std::fs::write(own.join("themes/Day"), "background = ffffff\n").unwrap();
        std::fs::write(own.join("themes/Night"), "background = 101010\n").unwrap();
        std::fs::write(own.join("themes/Preview"), "background = 777777\n").unwrap();
        let loader = ConfigLoader {
            home: directory.clone(),
            app_config_home: None,
            xdg_config_home: directory.join("config"),
            resources_dir: None,
            dark_mode: true,
            working_directory: directory.clone(),
        };
        let mut appearance = Appearance::new(Theme::Dark);
        for (override_, system, expected) in [
            (None, Theme::Dark, Theme::Light),
            (Some(WindowTheme::System), Theme::Dark, Theme::Dark),
            (Some(WindowTheme::System), Theme::Light, Theme::Light),
            (Some(WindowTheme::Dark), Theme::Light, Theme::Dark),
            (Some(WindowTheme::Light), Theme::Dark, Theme::Light),
            (None, Theme::Dark, Theme::Light),
        ] {
            appearance.override_ = override_;
            appearance.system = system;
            for scoped in [None, Some("light:Day,dark:Night")] {
                let loaded = appearance.load(&loader, &[], scoped);
                assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
                assert_eq!(
                    loaded.config.background,
                    if expected == Theme::Dark {
                        Rgb::new(16, 16, 16)
                    } else {
                        Rgb::new(255, 255, 255)
                    }
                );
                assert_eq!(appearance.theme(&loaded.config), expected);
            }
            let preview = appearance.load(&loader, &[], Some("Preview"));
            assert_eq!(appearance.theme(&preview.config), expected);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        appearance.override_ = Some(WindowTheme::Light);
        std::fs::write(
            &path,
            original.replace("window-theme = light", "window-theme = dark"),
        )
        .unwrap();
        assert_eq!(
            appearance.theme(&appearance.load(&loader, &[], None).config),
            Theme::Light
        );
        appearance.override_ = None;
        let latest = appearance.load(&loader, &[], None);
        assert_eq!(appearance.theme(&latest.config), Theme::Dark);
        assert_eq!(latest.config.background, Rgb::new(16, 16, 16));
        assert!(
            loader.dark_mode,
            "loading must not overwrite the source loader's OS state"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
