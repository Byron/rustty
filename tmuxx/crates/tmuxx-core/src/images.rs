use super::*;

impl Engine {
    pub(super) fn synchronize_images(&mut self) {
        let number = |name: &str, default| {
            self.server_options
                .get(name)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(default)
        };
        let size = number("image-size-limit", 64 * 1024 * 1024);
        self.image_quota.set_limits(
            number("image-total-count-limit", 1000),
            number("image-total-size-limit", 1024 * 1024 * 1024),
        );
        let owners = self
            .panes
            .values()
            .filter(|pane| pane.terminal.uses_image_quota(&self.image_quota))
            .flat_map(|pane| pane.terminal.image_owners())
            .collect();
        self.image_quota.retain_owners(&owners);
        for pane in self.panes.values_mut() {
            pane.terminal.set_image_size_limit(size);
            pane.terminal.set_image_quota(self.image_quota.clone());
            pane.terminal.prune_images();
        }
    }
}

// Runtime spawn rollback needs an independent image budget and eviction history.
impl Clone for Engine {
    fn clone(&self) -> Self {
        let image_quota = self.image_quota.fork();
        let mut panes = self.panes.clone();
        for pane in panes.values_mut() {
            if pane.terminal.uses_image_quota(&self.image_quota) {
                pane.terminal.rebind_image_quota(image_quota.clone());
            }
        }
        Self {
            now: self.now,
            started: self.started,
            identity: self.identity.clone(),
            default_child_path: self.default_child_path.clone(),
            sessions: self.sessions.clone(),
            windows: self.windows.clone(),
            panes,
            options: self.options.clone(),
            environment: self.environment.clone(),
            hidden_environment: self.hidden_environment.clone(),
            removed_environment: self.removed_environment.clone(),
            server_options: self.server_options.clone(),
            global_window_options: self.global_window_options.clone(),
            user_hook_names: self.user_hook_names.clone(),
            monitored_user_options: self.monitored_user_options.clone(),
            buffers: self.buffers.clone(),
            buffer_created: self.buffer_created.clone(),
            clients: self.clients.clone(),
            format_jobs: self.format_jobs.clone(),
            image_quota,
            automatic_buffers: self.automatic_buffers.clone(),
            buffer_order: self.buffer_order.clone(),
            sequence: self.sequence,
            marked: self.marked,
            next_link: self.next_link,
            next_session: self.next_session,
            next_window: self.next_window,
            next_pane: self.next_pane,
            next_buffer: self.next_buffer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(engine: &mut Engine, context: &mut ClientContext, source: &str) {
        for command in tmuxx_command::parse(source.as_bytes()).unwrap() {
            engine.execute(&command, context).unwrap();
        }
    }
    #[test]
    fn budgets_span_panes_and_engine_checkpoints_do_not_share_evictions() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        command(&mut engine, &mut context, "new-session -d -s image-test ''");
        command(&mut engine, &mut context, "split-window -d ''");
        command(
            &mut engine,
            &mut context,
            "set-option -s image-total-count-limit 1",
        );
        engine.feed(0, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AAAA/w==\x1b\\");
        assert_eq!(
            engine.panes[&0]
                .terminal
                .snapshot()
                .primary_graphics
                .images
                .len(),
            1
        );
        let mut checkpoint = engine.clone();
        checkpoint.feed(1, b"\x1b_Ga=t,i=2,f=32,s=1,v=1;AAAA/w==\x1b\\");
        assert!(
            checkpoint.panes[&0]
                .terminal
                .snapshot()
                .primary_graphics
                .images
                .is_empty()
        );
        assert_eq!(
            engine.panes[&0]
                .terminal
                .snapshot()
                .primary_graphics
                .images
                .len(),
            1
        );
        engine.feed(1, b"\x1b_Ga=t,i=3,f=32,s=1,v=1;AAAA/w==\x1b\\");
        assert!(
            engine.panes[&0]
                .terminal
                .snapshot()
                .primary_graphics
                .images
                .is_empty()
        );
        assert_eq!(
            engine.panes[&1].terminal.snapshot().primary_graphics.images[0].id,
            3
        );
        assert_eq!(
            checkpoint.panes[&1]
                .terminal
                .snapshot()
                .primary_graphics
                .images[0]
                .id,
            2
        );
    }
    #[test]
    fn respawn_drops_the_previous_screen_budget_before_the_next_admission() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        command(&mut engine, &mut context, "new-session -d -s image-test ''");
        command(&mut engine, &mut context, "split-window -d ''");
        command(
            &mut engine,
            &mut context,
            "set-option -s image-total-count-limit 2",
        );
        engine.feed(1, b"\x1b_Ga=t,i=2,f=32,s=1,v=1;AAAA/w==\x1b\\");
        engine.feed(0, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AAAA/w==\x1b\\");
        command(&mut engine, &mut context, "respawn-pane -k -t %0 ''");
        engine.feed(0, b"\x1b_Ga=t,i=3,f=32,s=1,v=1;AAAA/w==\x1b\\");
        assert_eq!(
            engine.panes[&1].terminal.snapshot().primary_graphics.images[0].id,
            2
        );
        assert_eq!(
            engine.panes[&0].terminal.snapshot().primary_graphics.images[0].id,
            3
        );
    }
}
