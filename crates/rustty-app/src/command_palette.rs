//! A searchable command list with shortcuts from the active configuration.

use egui::{Color32, Key, RichText, Vec2};
use rustty::config::{Action, Config, Direction, KeyBinding, KeyTrigger, ThemeChoice};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PaletteEvent {
    Action(Action),
    PreviewTheme(ThemeChoice),
    AcceptTheme(ThemeChoice),
}

#[derive(Default)]
pub(super) struct CommandPalette {
    pub open: bool,
    query: String,
    selected: Option<usize>,
    themes: Option<Vec<ThemeChoice>>,
    last_preview: Option<std::path::PathBuf>,
    pointer_position: Option<egui::Pos2>,
}

impl CommandPalette {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.selected = None;
        self.themes = None;
        self.last_preview = None;
    }

    pub fn show_themes(&mut self, themes: Vec<ThemeChoice>) {
        self.open = true;
        self.query.clear();
        self.selected = Some(0);
        self.themes = Some(themes);
        self.last_preview = None;
    }

    pub fn show(
        &mut self,
        root: &mut egui::Ui,
        id: egui::Id,
        config: &Config,
        request_focus: &mut bool,
    ) -> Option<PaletteEvent> {
        if root.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.open = false;
            return None;
        }
        let (pointer_position, pointer_moved) = root.input(|input| {
            let position = input.pointer.hover_pos();
            let moved = position != self.pointer_position
                && input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::PointerMoved(_)));
            (position, moved)
        });
        self.pointer_position = pointer_position;
        let bounds = root.max_rect().shrink(16.0);
        let top = bounds.top() + bounds.height() * 0.05;
        let width = bounds.width().clamp(1.0, 500.0);
        let background = Color32::from_rgb(
            config.background.r,
            config.background.g,
            config.background.b,
        );
        let foreground = Color32::from_rgb(
            config.foreground.r,
            config.foreground.g,
            config.foreground.b,
        );
        let fill = background.lerp_to_gamma(foreground, 0.08);
        let frame = egui::Frame::popup(root.style())
            .fill(fill)
            .corner_radius(10)
            .inner_margin(0);
        let mut action = None;
        let response = egui::Area::new(id)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(bounds.center().x, top))
            .default_width(width)
            .constrain_to(bounds)
            .fade_in(false)
            .show(root.ctx(), |ui| {
                // An Area remembers its last size; let the results grow after filtering.
                ui.set_max_height((bounds.bottom() - top).max(1.0));
                ui.visuals_mut().override_text_color = Some(foreground);
                ui.visuals_mut().weak_text_alpha = 0.65;
                let cursor = &mut ui.visuals_mut().text_cursor;
                cursor.stroke = egui::Stroke::new(2.0, foreground);
                cursor.blink = true;
                let field_id = id.with("query");
                if *request_focus {
                    if !ui.memory(|memory| memory.has_focus(field_id)) {
                        ui.memory_mut(|memory| memory.request_focus(field_id));
                    }
                    if !ui.is_sizing_pass() {
                        *request_focus = false;
                    }
                }
                // Consume vertical navigation before the single-line editor does.
                let (up, down) = ui.input_mut(|input| {
                    (
                        input.consume_key(egui::Modifiers::NONE, Key::ArrowUp)
                            | input.consume_key(egui::Modifiers::CTRL, Key::P),
                        input.consume_key(egui::Modifiers::NONE, Key::ArrowDown)
                            | input.consume_key(egui::Modifiers::CTRL, Key::N),
                    )
                });
                if up || down {
                    ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                }
                frame.show(ui, |ui| {
                    ui.set_width((width - frame.total_margin().sum().x).max(1.0));
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let hint = if self.themes.is_some() {
                        "Select a theme"
                    } else {
                        "Execute a command"
                    };
                    let query = ui.add_sized(
                        [ui.available_width(), 48.0],
                        egui::TextEdit::singleline(&mut self.query)
                            .id(field_id)
                            .hint_text(RichText::new(format!("{hint}…")).size(20.0))
                            .font(egui::FontId::proportional(20.0))
                            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 0)))
                            .vertical_align(egui::Align::Center),
                    );
                    query.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), hint)
                    });
                    let needle = self.query.trim();
                    let mut commands = if self.themes.is_none() {
                        actions()
                    } else {
                        Vec::new()
                    };
                    commands.retain(|(label, _)| matches_query(label, needle));
                    let themes = self.themes.as_ref().map(|themes| {
                        themes
                            .iter()
                            .filter(|theme| matches_query(&theme.name, needle))
                            .collect::<Vec<_>>()
                    });
                    let count = themes.as_ref().map_or(commands.len(), Vec::len);
                    let event_at = |index: usize| {
                        if let Some(themes) = &themes {
                            themes
                                .get(index)
                                .map(|theme| PaletteEvent::AcceptTheme((*theme).clone()))
                        } else {
                            commands
                                .get(index)
                                .map(|(_, action)| PaletteEvent::Action(action.clone()))
                        }
                    };
                    if query.changed() {
                        self.selected = (themes.is_some() || !needle.is_empty()).then_some(0);
                    }
                    self.selected = self.selected.filter(|index| *index < count);
                    if count > 0 {
                        if down {
                            self.selected = Some(self.selected.map_or(0, |i| (i + 1) % count));
                        } else if up {
                            self.selected = Some(
                                self.selected
                                    .unwrap_or(0)
                                    .checked_sub(1)
                                    .unwrap_or(count - 1),
                            );
                        }
                    }
                    let enter =
                        query.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                    if enter {
                        action = self.selected.and_then(event_at);
                        if action.is_none() {
                            query.request_focus();
                        }
                    }
                    ui.add(egui::Separator::default().spacing(1.0));
                    egui::Frame::NONE.inner_margin(8).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.spacing_mut().button_padding = Vec2::new(10.0, 6.0);
                        ui.visuals_mut().selection.bg_fill =
                            root.visuals().selection.bg_fill.gamma_multiply(0.2);
                        let mut scroll = egui::ScrollArea::vertical()
                            .max_height((bounds.bottom() - top - 67.0).clamp(1.0, 176.0))
                            .min_scrolled_height(1.0)
                            .animated(false)
                            .auto_shrink([false, true]);
                        if query.changed() {
                            scroll = scroll.vertical_scroll_offset(0.0);
                        }
                        scroll.show(ui, |ui| {
                            if count == 0 {
                                ui.add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Label::new(RichText::new("No matches").weak()),
                                );
                            }
                            // A resting pointer must not undo keyboard or search selection.
                            let pointer_moved = pointer_moved && !up && !down && !query.changed();
                            for index in 0..count {
                                let (label, shortcut) = if let Some(themes) = &themes {
                                    let theme = themes[index];
                                    (
                                        theme.name.as_str(),
                                        if theme.user { "User" } else { "Bundled" }.to_owned(),
                                    )
                                } else {
                                    let (label, command) = &commands[index];
                                    (
                                        *label,
                                        shortcut_label(&config.keybinds, command)
                                            .unwrap_or_default(),
                                    )
                                };
                                let selected = self.selected == Some(index);
                                let row = ui.add(
                                    egui::Button::selectable(
                                        selected,
                                        RichText::new(label)
                                            .size(14.0)
                                            .color(ui.visuals().text_color()),
                                    )
                                    .shortcut_text(RichText::new(&shortcut).size(14.0))
                                    .min_size(Vec2::new(ui.available_width(), 32.0))
                                    .stroke(egui::Stroke::NONE)
                                    .corner_radius(5)
                                    .truncate(),
                                );
                                if selected && (up || down || query.changed()) {
                                    row.scroll_to_me(None);
                                }
                                if themes.is_some() && pointer_moved && row.hovered() {
                                    self.selected = Some(index);
                                    ui.ctx().request_repaint();
                                }
                                if row.clicked() {
                                    action = event_at(index);
                                }
                                if !shortcut.is_empty() {
                                    row.on_hover_text(format!("{label}  {shortcut}"));
                                }
                            }
                        });
                    });
                    if action.is_none()
                        && !ui.is_sizing_pass()
                        && let Some(theme) =
                            self.selected.and_then(|index| themes.as_ref()?.get(index))
                        && self.last_preview.as_ref() != Some(&theme.path)
                    {
                        self.last_preview = Some(theme.path.clone());
                        action = Some(PaletteEvent::PreviewTheme((*theme).clone()));
                    }
                });
                if matches!(
                    action,
                    Some(PaletteEvent::Action(_) | PaletteEvent::AcceptTheme(_))
                ) {
                    self.open = false;
                }
            });
        if response.response.clicked_elsewhere() {
            self.open = false;
            root.ctx().request_repaint();
            return None;
        }
        action
    }
}

fn matches_query(label: &str, query: &str) -> bool {
    let mut letters = label.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .all(|needle| letters.by_ref().any(|c| c == needle))
}

fn shortcut_label(bindings: &[KeyBinding], action: &Action) -> Option<String> {
    let binding = bindings.iter().rev().find(|binding| {
        binding.table.is_none()
            && binding.actions == std::slice::from_ref(action)
            && binding
                .trigger
                .iter()
                .all(|trigger| trigger.key != "catch_all")
    })?;
    Some(
        binding
            .trigger
            .iter()
            .map(trigger_label)
            .collect::<Vec<_>>()
            .join(" → "),
    )
}

fn trigger_label(trigger: &KeyTrigger) -> String {
    let mut label = String::new();
    for (enabled, symbol, name) in [
        (trigger.modifiers.control, "⌃", "Ctrl+"),
        (trigger.modifiers.alt, "⌥", "Alt+"),
        (trigger.modifiers.shift, "⇧", "Shift+"),
        (trigger.modifiers.super_key, "⌘", "Win+"),
    ] {
        if enabled {
            label.push_str(if cfg!(windows) { name } else { symbol });
        }
    }
    let key = trigger
        .key
        .strip_prefix("key_")
        .or_else(|| trigger.key.strip_prefix("digit_"))
        .unwrap_or(&trigger.key);
    let key = if let Some(key) = key.strip_prefix("kp_") {
        label.push_str("Keypad ");
        key
    } else {
        key
    };
    // Windows key names avoid relying on glyphs for macOS keyboard symbols.
    let (symbol, name) = match key {
        "arrow_left" => ("←", "Left"),
        "arrow_right" => ("→", "Right"),
        "arrow_up" => ("↑", "Up"),
        "arrow_down" => ("↓", "Down"),
        "enter" => ("↩", "Enter"),
        "escape" => ("⎋", "Esc"),
        "tab" => ("⇥", "Tab"),
        "backspace" => ("⌫", "Backspace"),
        "delete" => ("⌦", "Delete"),
        "home" => ("↖", "Home"),
        "end" => ("↘", "End"),
        "page_up" => ("⇞", "Page Up"),
        "page_down" => ("⇟", "Page Down"),
        "space" | " " => ("Space", "Space"),
        "backquote" => ("`", "`"),
        "caps_lock" => ("⇪", "Caps Lock"),
        "add" => ("+", "+"),
        "subtract" => ("-", "-"),
        "multiply" => ("*", "*"),
        "divide" => ("/", "/"),
        "decimal" => (".", "."),
        "equal" => ("=", "="),
        key => {
            label.push_str(&key.replace('_', " ").to_uppercase());
            return label;
        }
    };
    label.push_str(if cfg!(windows) { name } else { symbol });
    label
}

fn actions() -> Vec<(&'static str, Action)> {
    let mut actions = vec![
        ("New Window", Action::NewWindow),
        ("New Tab", Action::NewTab),
        ("Split Right", Action::NewSplit(Direction::Right)),
        ("Split Down", Action::NewSplit(Direction::Down)),
        ("Zoom Pane", Action::ToggleSplitZoom),
        ("Zoom Quadrant", Action::ToggleQuadrantZoom),
        ("Equalize Splits", Action::EqualizeSplits),
        ("Find", Action::StartSearch),
        ("Open Configuration", Action::OpenConfig),
        ("Open Saved Layout", Action::OpenLayout),
        ("Reload Configuration", Action::ReloadConfig),
        ("Reset Terminal", Action::Reset),
        ("Themes…", Action::Themes),
        ("Toggle Quick Terminal", Action::ToggleQuickTerminal),
        ("Toggle Outliner", Action::ToggleOutliner),
        ("Toggle Pin", Action::TogglePin),
        ("Undo Layout Change", Action::Undo),
        ("Redo Layout Change", Action::Redo),
    ];
    actions.sort_by_key(|(label, _)| *label);
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::ConfigLoader;

    #[test]
    fn shortcuts_follow_loaded_bindings_including_remaps_unbinds_and_sequences() {
        let home = std::env::temp_dir().join(format!("rustty-palette-{}", std::process::id()));
        let loader = ConfigLoader {
            app_config_home: None,
            xdg_config_home: home.join(".config"),
            resources_dir: None,
            dark_mode: true,
            working_directory: home.clone(),
            home,
        };
        let load = |args: &[&str]| {
            let loaded = loader
                .load_with_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
            loaded.config.keybinds
        };
        let defaults = load(&[]);
        assert_eq!(
            shortcut_label(&defaults, &Action::NewWindow).as_deref(),
            Some(if cfg!(windows) {
                "Ctrl+Shift+N"
            } else {
                "⌘N"
            })
        );
        assert_eq!(
            shortcut_label(&defaults, &Action::CloseAllWindows).as_deref(),
            Some(if cfg!(windows) {
                "Ctrl+Alt+Shift+W"
            } else {
                "⌥⇧⌘W"
            })
        );
        assert_eq!(shortcut_label(&defaults, &Action::OpenLayout), None);
        let remapped = load(&[
            "--keybind=super+n=unbind",
            "--keybind=ctrl+shift+n=unbind",
            "--keybind=ctrl+alt+n=new_window",
            "--keybind=menu/super+p=new_window",
        ]);
        assert_eq!(
            shortcut_label(&remapped, &Action::NewWindow).as_deref(),
            Some(if cfg!(windows) {
                "Ctrl+Alt+N"
            } else {
                "⌃⌥N"
            })
        );
        assert_eq!(
            shortcut_label(
                &load(&["--keybind=super+n=unbind", "--keybind=ctrl+shift+n=unbind"]),
                &Action::NewWindow,
            ),
            None
        );
        assert_eq!(
            shortcut_label(&load(&["--keybind=clear"]), &Action::NewWindow),
            None
        );
        let sequence = load(&[
            "--keybind=clear",
            "--keybind=super+k>physical:shift+key_n=new_window",
        ]);
        assert_eq!(
            shortcut_label(&sequence, &Action::NewWindow).as_deref(),
            Some(if cfg!(windows) {
                "Win+K → Shift+N"
            } else {
                "⌘K → ⇧N"
            })
        );
        let chained = load(&[
            "--keybind=clear",
            "--keybind=super+n=new_window",
            "--keybind=chain=new_tab",
            "--keybind=catch_all=new_window",
        ]);
        assert_eq!(shortcut_label(&chained, &Action::NewWindow), None);
        let splits = load(&[]);
        assert_eq!(
            shortcut_label(&splits, &Action::NewSplit(Direction::Right)).as_deref(),
            Some(if cfg!(windows) {
                "Ctrl+Shift+D"
            } else {
                "⌘D"
            })
        );
        assert_eq!(
            shortcut_label(&splits, &Action::NewSplit(Direction::Down)).as_deref(),
            Some(if cfg!(windows) {
                "Ctrl+Alt+D"
            } else {
                "⇧⌘D"
            })
        );
        for (trigger, symbol, name) in [
            ("super+shift+enter", "⇧⌘↩", "Shift+Win+Enter"),
            ("super+alt+arrow_left", "⌥⌘←", "Alt+Win+Left"),
            ("physical:super+digit_1", "⌘1", "Win+1"),
            ("super++", "⌘+", "Win++"),
            ("ctrl+space", "⌃Space", "Ctrl+Space"),
            ("super+backquote", "⌘`", "Win+`"),
            ("f12", "F12", "F12"),
            ("super+kp_enter", "⌘Keypad ↩", "Win+Keypad Enter"),
        ] {
            assert_eq!(
                trigger_label(&KeyTrigger::parse(trigger).unwrap()),
                if cfg!(windows) { name } else { symbol }
            );
        }
    }

    fn draw(
        context: &egui::Context,
        palette: &mut CommandPalette,
        config: &Config,
        focus: &mut bool,
        events: Vec<egui::Event>,
    ) -> (Option<PaletteEvent>, egui::FullOutput) {
        let mut action = None;
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(800.0, 400.0),
            )),
            focused: true,
            events,
            ..Default::default()
        };
        rustty_app::input::filter_egui_events(&mut raw, true);
        let mut output = context.run_ui(raw, |root| {
            action = palette.show(root, egui::Id::new("palette"), config, focus)
        });
        output.textures_delta.clear();
        (action, output)
    }

    fn key(key: Key) -> egui::Event {
        modified_key(key, egui::Modifiers::NONE)
    }

    fn modified_key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn themes() -> Vec<ThemeChoice> {
        ["Alpha", "Beta", "Gamma"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| ThemeChoice {
                name: name.to_owned(),
                path: std::path::PathBuf::from("/themes").join(name),
                user: index == 0,
            })
            .collect()
    }

    #[test]
    fn themes_preview_changed_choices_through_navigation_and_search_before_accepting() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        let choices = themes();
        palette.show_themes(choices.clone());
        let events = (0..3)
            .filter_map(|_| draw(&context, &mut palette, &config, &mut focus, vec![]).0)
            .collect::<Vec<_>>();
        assert_eq!(events, [PaletteEvent::PreviewTheme(choices[0].clone())]);
        assert!(palette.open);
        assert_eq!(palette.selected, Some(0));
        let (_, output) = draw(&context, &mut palette, &config, &mut focus, vec![]);
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for label in ["Select a theme…", "User", "Bundled"] {
            assert!(labels.contains(&label), "{labels:?}");
        }
        for (key, index) in [(Key::ArrowDown, 1), (Key::ArrowUp, 0), (Key::ArrowUp, 2)] {
            let (event, _) = draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![self::key(key)],
            );
            assert_eq!(
                event,
                Some(PaletteEvent::PreviewTheme(choices[index].clone()))
            );
            assert!(palette.open);
            assert_eq!(palette.selected, Some(index));
        }
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("bta".into())],
        );
        assert_eq!(event, Some(PaletteEvent::PreviewTheme(choices[1].clone())));
        assert_eq!(palette.selected, Some(0));
        assert_eq!(
            draw(&context, &mut palette, &config, &mut focus, vec![]).0,
            None
        );
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(event, Some(PaletteEvent::AcceptTheme(choices[1].clone())));
        assert!(!palette.open);
    }

    #[test]
    fn themes_preview_moving_pointer_without_overriding_keyboard_and_accept_clicks() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        let choices = themes();
        palette.show_themes(choices.clone());
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        let (_, output) = draw(&context, &mut palette, &config, &mut focus, vec![]);
        let position = |name| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == name => {
                        Some(text.pos + text.galley.rect.center().to_vec2())
                    }
                    _ => None,
                })
                .unwrap()
        };
        let alpha = position("Alpha");
        let beta = position("Beta");
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::PointerMoved(beta)],
        );
        assert_eq!(event, Some(PaletteEvent::PreviewTheme(choices[1].clone())));
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::ArrowUp)],
        );
        assert_eq!(event, Some(PaletteEvent::PreviewTheme(choices[0].clone())));
        assert_eq!(
            draw(&context, &mut palette, &config, &mut focus, vec![]).0,
            None
        );
        assert_eq!(palette.selected, Some(0));
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::PointerMoved(beta)],
        );
        assert_eq!(event, None);
        assert_eq!(palette.selected, Some(0));
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::PointerMoved(beta + Vec2::new(1.0, 0.0))],
        );
        assert_eq!(event, Some(PaletteEvent::PreviewTheme(choices[1].clone())));
        for pressed in [true, false] {
            let (event, _) = draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![
                    egui::Event::PointerMoved(alpha),
                    egui::Event::PointerButton {
                        pos: alpha,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(
                event,
                Some(if pressed {
                    PaletteEvent::PreviewTheme(choices[0].clone())
                } else {
                    PaletteEvent::AcceptTheme(choices[0].clone())
                })
            );
        }
        assert!(!palette.open);
    }

    #[test]
    fn themes_cancel_without_a_preview_and_toggle_restores_commands() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        palette.show_themes(themes());
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::ArrowDown), key(Key::Escape)],
        );
        assert_eq!(event, None);
        assert!(!palette.open);
        palette.toggle();
        assert!(palette.open);
        assert!(palette.themes.is_none());
        assert!(palette.last_preview.is_none());
        assert_eq!(palette.selected, None);
        focus = true;
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("themes".into())],
        );
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(event, Some(PaletteEvent::Action(Action::Themes)));
    }

    #[test]
    fn themes_with_no_matches_keep_focus_and_clearing_search_restores_first_preview() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        let choices = themes();
        palette.show_themes(choices.clone());
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        assert!(context.text_edit_focused(), "before navigation");
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::ArrowDown)],
        );
        assert!(context.text_edit_focused(), "after navigation");
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("no matches".into())],
        );
        assert_eq!(palette.query, "no matches");
        assert_eq!(palette.selected, None);
        assert_eq!(event, None);
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(event, None);
        assert!(palette.open);
        assert!(context.text_edit_focused());
        assert_eq!(palette.selected, None);
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![
                modified_key(Key::A, egui::Modifiers::COMMAND),
                key(Key::Backspace),
            ],
        );
        assert_eq!(event, Some(PaletteEvent::PreviewTheme(choices[0].clone())));
        assert_eq!(palette.selected, Some(0));

        palette.show_themes(Vec::new());
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        let (event, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(event, None);
        assert!(palette.open);
        assert!(context.text_edit_focused());
    }

    #[test]
    fn palette_exposes_recovery_outliner_and_pin_without_sending_terminal_input() {
        for (query, command, expected) in [
            ("reset terminal", "reset", Action::Reset),
            ("outliner", "toggle_outliner", Action::ToggleOutliner),
            ("toggle pin", "toggle_pin", Action::TogglePin),
        ] {
            let context = egui::Context::default();
            let config = Config::default();
            let mut palette = CommandPalette::default();
            let mut focus = true;
            palette.toggle();
            draw(&context, &mut palette, &config, &mut focus, vec![]);
            draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![egui::Event::Text(query.into())],
            );
            let (action, _) = draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![key(Key::Enter)],
            );
            assert_eq!(action, Some(PaletteEvent::Action(expected.clone())));
            assert_eq!(Action::parse(command).unwrap(), expected);
        }
    }

    #[test]
    fn fuzzy_queries_allow_gaps_case_differences_and_word_abbreviations() {
        for (label, query, expected) in [
            ("New Window", "nwndw", true),
            ("New Window", " N W ", true),
            ("Open Configuration", "cfg", true),
            ("Reload Configuration", "cfg", true),
            ("Split Right", "SP R", true),
            ("Zoom Quadrant", "zq", true),
            ("New Window", "nnn", false),
            ("Split Right", "right split", false),
            ("New Tab", " \t ", true),
        ] {
            assert_eq!(
                matches_query(label, query),
                expected,
                "{label:?}, {query:?}"
            );
        }
    }

    #[test]
    fn palette_filters_navigates_submits_and_dismisses_without_losing_text_focus() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        palette.toggle();
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        let (_, output) = draw(&context, &mut palette, &config, &mut focus, vec![]);
        assert!(output.platform_output.ime.is_some());
        assert!(!focus);
        assert!(palette.selected.is_none());
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(labels.contains(&"New Window"), "{labels:?}");
        assert!(
            labels.contains(&if cfg!(windows) {
                "Ctrl+Shift+N"
            } else {
                "⌘N"
            }),
            "{labels:?}"
        );
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::ArrowUp)],
        );
        assert_eq!(palette.selected, Some(actions().len() - 1));
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("NEW ".into())],
        );
        assert_eq!(palette.selected, Some(0));
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::ArrowDown)],
        );
        assert_eq!(palette.selected, Some(1));
        let (action, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(action, Some(PaletteEvent::Action(Action::NewWindow)));
        assert!(!palette.open);

        palette.toggle();
        focus = true;
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("no such command".into())],
        );
        let (action, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(action, None);
        assert!(palette.open);
        assert!(context.text_edit_focused());
        assert_eq!(palette.selected, None);
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Escape)],
        );
        assert!(!palette.open);

        palette.toggle();
        focus = true;
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("new".into())],
        );
        assert_eq!(palette.selected, Some(0));
        let (action, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![modified_key(Key::OpenBracket, egui::Modifiers::CTRL)],
        );
        assert_eq!(action, None);
        assert!(!palette.open);
    }

    #[test]
    fn clicking_outside_cancels_commands_and_themes_but_query_clicks_stay_open() {
        for theme_picker in [false, true] {
            let context = egui::Context::default();
            let config = Config::default();
            let mut palette = CommandPalette::default();
            let mut focus = true;
            if theme_picker {
                palette.show_themes(themes());
            } else {
                palette.toggle();
            }
            draw(&context, &mut palette, &config, &mut focus, vec![]);
            draw(&context, &mut palette, &config, &mut focus, vec![]);
            let query = context
                .read_response(egui::Id::new("palette").with("query"))
                .unwrap()
                .rect
                .center();
            for (pos, stays_open) in [(query, true), (egui::pos2(20.0, 380.0), false)] {
                draw(
                    &context,
                    &mut palette,
                    &config,
                    &mut focus,
                    vec![egui::Event::PointerMoved(pos)],
                );
                assert!(palette.open, "moving outside must not dismiss the palette");
                for pressed in [true, false] {
                    let (event, _) = draw(
                        &context,
                        &mut palette,
                        &config,
                        &mut focus,
                        vec![egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                    assert_eq!(
                        event, None,
                        "dismissal must not execute a command or accept a theme"
                    );
                    if pressed {
                        assert!(palette.open, "consume the full click before dismissing");
                    }
                }
                assert_eq!(palette.open, stays_open);
            }
        }
    }

    #[test]
    fn control_navigation_wraps_matches_and_preserves_text_editing() {
        let context = egui::Context::default();
        context.set_os(egui::os::OperatingSystem::Mac);
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        palette.toggle();
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        for (key, selected) in [(Key::N, 0), (Key::P, actions().len() - 1), (Key::N, 0)] {
            draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![modified_key(key, egui::Modifiers::CTRL)],
            );
            assert_eq!(palette.selected, Some(selected));
            assert!(palette.query.is_empty());
        }
        draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![egui::Event::Text("cfg".into())],
        );
        for (key, selected) in [(Key::N, 1), (Key::N, 0), (Key::P, 1)] {
            let (_, output) = draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![modified_key(key, egui::Modifiers::CTRL)],
            );
            assert_eq!(palette.selected, Some(selected));
            assert_eq!(palette.query, "cfg");
            assert!(output.platform_output.ime.is_some());
            let state =
                egui::TextEdit::load_state(&context, egui::Id::new("palette").with("query"))
                    .unwrap();
            assert_eq!(state.cursor.char_range().unwrap().primary.index.0, 3);
        }
        // Other macOS Emacs motions still belong to the search editor.
        for (key, index) in [(Key::A, 0), (Key::F, 1), (Key::B, 0), (Key::E, 3)] {
            draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![modified_key(key, egui::Modifiers::CTRL)],
            );
            let state =
                egui::TextEdit::load_state(&context, egui::Id::new("palette").with("query"))
                    .unwrap();
            assert_eq!(state.cursor.char_range().unwrap().primary.index.0, index);
            assert_eq!(palette.selected, Some(1));
        }
        let (action, _) = draw(
            &context,
            &mut palette,
            &config,
            &mut focus,
            vec![key(Key::Enter)],
        );
        assert_eq!(action, Some(PaletteEvent::Action(Action::ReloadConfig)));
    }

    #[test]
    fn palette_expands_after_a_search_with_one_or_no_matches() {
        let context = egui::Context::default();
        let config = Config::default();
        let mut palette = CommandPalette::default();
        let mut focus = true;
        palette.toggle();
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        draw(&context, &mut palette, &config, &mut focus, vec![]);
        for (query, expected) in [
            ("New Window", vec!["New Window"]),
            ("new", vec!["New Tab", "New Window"]),
            ("no such command", vec!["No matches"]),
            ("c f g", vec!["Open Configuration", "Reload Configuration"]),
            (
                "layout",
                vec![
                    "Open Saved Layout",
                    "Redo Layout Change",
                    "Undo Layout Change",
                ],
            ),
            (
                "o",
                vec![
                    "New Window",
                    "Open Configuration",
                    "Open Saved Layout",
                    "Redo Layout Change",
                    "Reload Configuration",
                ],
            ),
            (
                "",
                vec![
                    "Equalize Splits",
                    "Find",
                    "New Tab",
                    "New Window",
                    "Open Configuration",
                ],
            ),
        ] {
            // Replace through the editor, so selection and scrolling follow real typing.
            let replace = if query.is_empty() {
                key(Key::Backspace)
            } else {
                egui::Event::Text(query.into())
            };
            draw(
                &context,
                &mut palette,
                &config,
                &mut focus,
                vec![modified_key(Key::A, egui::Modifiers::COMMAND), replace],
            );
            for _ in 0..20 {
                draw(&context, &mut palette, &config, &mut focus, vec![]);
            }
            let (_, output) = draw(&context, &mut palette, &config, &mut focus, vec![]);
            let visible = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if shape
                            .clip_rect
                            .contains_rect(text.galley.rect.translate(text.pos.to_vec2())) =>
                    {
                        Some(text.galley.job.text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for label in expected {
                assert!(
                    visible.contains(&label),
                    "{query:?} hid {label:?}: {visible:?}"
                );
            }
            if query == "o" {
                // Clearing the query must also cancel scrolling to an old match.
                draw(
                    &context,
                    &mut palette,
                    &config,
                    &mut focus,
                    vec![modified_key(Key::P, egui::Modifiers::CTRL)],
                );
            }
        }
    }

    #[test]
    fn palette_stays_inside_small_windows() {
        use egui::emath::GuiRounding as _;

        for (size, scale) in [Vec2::new(800.0, 400.0), Vec2::new(240.0, 120.0)]
            .into_iter()
            .flat_map(|size| [1.0, 1.25, 1.5, 2.0].map(|scale| (size, scale)))
        {
            let context = egui::Context::default();
            let config = Config::default();
            let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let id = egui::Id::new("palette");
            let mut palette = CommandPalette::default();
            let mut focus = true;
            palette.toggle();
            for _ in 0..3 {
                let mut input = egui::RawInput {
                    screen_rect: Some(bounds),
                    focused: true,
                    ..Default::default()
                };
                input
                    .viewports
                    .get_mut(&egui::ViewportId::ROOT)
                    .unwrap()
                    .native_pixels_per_point = Some(scale);
                let mut output = context.run_ui(input, |root| {
                    palette.show(root, id, &config, &mut focus);
                });
                output.textures_delta.clear();
            }
            let panel = context.read_response(id).unwrap().rect;
            // Text and container edges snap independently to native pixels.
            // Compare those pixels, allowing egui's additional UI-point rounding.
            let scale = context.pixels_per_point();
            let panel_pixels = (panel * scale).round_to_pixels(1.0);
            let bounds_pixels = (bounds.shrink(16.0) * scale).round_to_pixels(1.0);
            assert!(
                bounds_pixels.expand(1.0).contains_rect(panel_pixels),
                "{panel:?} outside {bounds:?} at {scale}x"
            );
        }
    }
}
