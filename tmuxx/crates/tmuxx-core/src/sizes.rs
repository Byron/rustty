use super::*;

impl Engine {
    pub(super) fn automatic_window_size(
        &self,
        session: Id,
        window: Id,
        context: &ClientContext,
        largest: bool,
    ) -> (u16, u16) {
        let mut size = None;
        for client in self.sizing_clients(context).filter(|client| {
            client
                .session
                .and_then(|session| self.sessions.get(&session))
                .is_some_and(|session| session.windows.values().any(|id| *id == window))
        }) {
            let (cols, rows) = client
                .window_sizes
                .get(&window)
                .copied()
                .unwrap_or((client.cols, client.rows));
            size = Some(match size {
                None => (cols, rows),
                Some((x, y)) if largest => (cols.max(x), rows.max(y)),
                Some((x, y)) => (cols.min(x), rows.min(y)),
            });
        }
        self.client_size_limit(
            window,
            context,
            size.unwrap_or_else(|| self.default_size(Some(session))),
        )
    }

    fn sizing_clients<'a>(
        &'a self,
        context: &'a ClientContext,
    ) -> impl Iterator<Item = &'a ClientContext> {
        let clients = self
            .clients
            .iter()
            .filter(move |client| client.client_name != context.client_name)
            .chain(std::iter::once(context));
        let unflagged = clients.clone().any(|client| {
            client.session.is_some()
                && (client.control || client.size_requested || client.window_size_requested)
                && !client.ignore_size
        });
        clients.filter(move |client| {
            client.session.is_some()
                && (!client.ignore_size || !unflagged)
                && (client.size_requested || client.window_size_requested)
        })
    }

    pub(super) fn new_window_size(
        &self,
        session: Id,
        context: &ClientContext,
    ) -> (u16, u16, u16, u16) {
        let policy = self.global_option("window-size").unwrap_or("latest");
        let clients: Vec<_> = self.sizing_clients(context).collect();
        let dimensions = |(cols, rows), (width, height)| {
            (
                cols,
                rows,
                if width == 0 { 16 } else { width },
                if height == 0 { 32 } else { height },
            )
        };
        if policy == "manual" {
            return dimensions(self.default_size(Some(session)), (0, 0));
        }
        if policy == "latest" && clients.iter().any(|client| std::ptr::eq(*client, context)) {
            return dimensions(
                (context.cols.clamp(1, 10000), context.rows.clamp(1, 10000)),
                (context.cell_width, context.cell_height),
            );
        }
        let mut size: Option<(u16, u16)> = None;
        let mut cell = (0, 0);
        for client in clients
            .into_iter()
            .filter(|client| client.session == Some(session))
        {
            if client.cell_width > cell.0 && client.cell_height > cell.1 {
                cell = (client.cell_width, client.cell_height);
            }
            size = Some(match size {
                None => (client.cols, client.rows),
                Some((cols, rows)) if policy == "largest" => {
                    (cols.max(client.cols), rows.max(client.rows))
                }
                Some((cols, rows)) => (cols.min(client.cols), rows.min(client.rows)),
            });
        }
        dimensions(
            size.unwrap_or_else(|| self.default_size(Some(session))),
            cell,
        )
    }

    pub(super) fn client_size_limit(
        &self,
        window: Id,
        context: &ClientContext,
        mut size: (u16, u16),
    ) -> (u16, u16) {
        let aggressive = self.window_option(window, "aggressive-resize") == Some("on");
        for client in self.sizing_clients(context) {
            let Some(session) = client.session.and_then(|id| self.sessions.get(&id)) else {
                continue;
            };
            if if aggressive {
                session.windows.get(&session.active) != Some(&window)
            } else {
                !session.windows.values().any(|id| *id == window)
            } {
                continue;
            }
            if let Some(&(cols, rows)) = client.window_sizes.get(&window) {
                size.0 = size.0.min(cols);
                size.1 = size.1.min(rows);
            }
        }
        size
    }
}
