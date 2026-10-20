use super::*;

impl ClientContext {
    pub(super) fn has_format_client(&self) -> bool {
        self.format_client
            .unwrap_or(self.created != 0 || self.control || !self.client_name.is_empty())
    }
}

impl Engine {
    pub(super) fn display_client(
        &self,
        target: Option<&str>,
        session: Option<Id>,
        context: &ClientContext,
    ) -> Result<ClientContext, String> {
        let selected = if let Some(target) = target {
            let name = target.strip_suffix(':').unwrap_or(target);
            Some(
                self.clients
                    .iter()
                    .find(|client| {
                        client.client_name == name
                            || client.identity.get("client_tty").is_some_and(|tty| {
                                tty == name || tty.strip_prefix("/dev/") == Some(name)
                            })
                    })
                    .ok_or_else(|| format!("can't find client: {target}"))?,
            )
        } else if context.control && context.session == session && session.is_some() {
            Some(context)
        } else if session.is_some() {
            let attached = self.clients.iter().any(|client| client.session == session);
            self.clients
                .iter()
                .filter(|client| {
                    client.session.is_some() && (!attached || client.session == session)
                })
                .max_by_key(|client| client.activity)
        } else {
            None
        };
        let mut result = selected.cloned().unwrap_or_else(|| context.clone());
        result.format_client = Some(selected.is_some());
        result.formats.clone_from(&context.formats);
        result.format_type = context.format_type;
        result.no_format_jobs = context.no_format_jobs;
        Ok(result)
    }

    pub(super) fn client_format(
        &self,
        key: &str,
        context: &ClientContext,
    ) -> Option<Option<String>> {
        // These callbacks return upstream templates even when the widget is absent.
        let template = match key {
            "buffer_mode_format" => Some("#{t/p:buffer_created}: #{buffer_sample}"),
            "client_mode_format" => Some(
                "#[fg=themelightgrey]#{t/p:client_activity}: session #[default]#{session_name}",
            ),
            "tree_mode_format" => Some(concat!(
                "#{?pane_format,",
                "#{?pane_marked,#[fg=thememagenta],}#{?pane_floating_flag,#[underscore],}",
                "#{pane_current_command}#[fg=themelightgrey]#{pane_flags}",
                "#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",}",
                ",window_format,#{?window_marked_flag,#[fg=thememagenta],}",
                "#{window_name}#[fg=themelightgrey]#{window_flags}",
                "#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",}",
                ",#[fg=themelightgrey]#{session_windows} windows",
                "#{?session_grouped, (group #{session_group}: #{session_group_list}),}",
                "#{?session_attached, (attached),}}"
            )),
            _ => None,
        };
        if let Some(template) = template {
            return Some(Some(template.into()));
        }
        if !matches!(
            key,
            "client_created"
                | "client_activity"
                | "client_control_mode"
                | "client_cell_width"
                | "client_cell_height"
                | "client_width"
                | "client_height"
                | "client_flags"
                | "client_name"
                | "client_session"
                | "client_readonly"
                | "client_pid"
                | "client_uid"
                | "client_user"
                | "client_termname"
                | "client_termtype"
                | "client_termfeatures"
                | "client_tty"
                | "client_utf8"
                | "client_last_session"
                | "client_key_table"
                | "client_prefix"
                | "client_colours"
                | "client_theme"
                | "client_written"
                | "client_discarded"
        ) {
            return None;
        }
        Some((|| {
            if !context.has_format_client() {
                return None;
            }
            Some(match key {
                "client_created" => (context.created / 1_000_000).to_string(),
                "client_activity" => (context.activity / 1_000_000).to_string(),
                "client_control_mode" => {
                    u8::from(context.control && !context.native_view).to_string()
                }
                "client_width" => context.cols.to_string(),
                "client_height" | "client_cell_width" | "client_cell_height" => {
                    if !context.native_view {
                        return None;
                    }
                    match key {
                        "client_height" => context.rows.to_string(),
                        "client_cell_width" => context.cell_width.to_string(),
                        _ => context.cell_height.to_string(),
                    }
                }
                "client_name" => context.client_name.clone(),
                "client_flags" => context.client_flags.clone(),
                "client_session" => self.sessions.get(&context.session?)?.name.clone(),
                "client_readonly" => u8::from(
                    context
                        .client_flags
                        .split(',')
                        .any(|flag| flag == "read-only"),
                )
                .to_string(),
                "client_key_table" => "root".into(),
                "client_prefix" => "0".into(),
                // C counts tty rendering bytes here, not command/control or native transport.
                "client_written" | "client_discarded" => "0".into(),
                "client_last_session" => self.sessions.get(&context.last_session?)?.name.clone(),
                _ => context.identity.get(key)?.clone(),
            })
        })())
    }
}
