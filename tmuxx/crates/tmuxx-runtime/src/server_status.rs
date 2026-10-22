use super::*;
use tmuxx_command::Command;
use tmuxx_protocol::native::ServerStatus;

impl Host {
    pub(super) fn is_management_client(&self, id: u64) -> bool {
        self.management_clients.contains(&id)
            && self
                .clients
                .get(&id)
                .is_some_and(|client| !client.subscribed)
            && !self.hooks.attached.contains(&id)
    }

    fn server_connection_count(&self, except: Option<u64>) -> usize {
        self.clients
            .keys()
            .filter(|&&id| {
                Some(id) != except
                    && !self.scheduler.is_internal(id)
                    && !self.is_management_client(id)
            })
            .count()
    }

    pub(super) fn refresh_server_status(&mut self) {
        if self.server_watchers.is_empty() {
            return;
        }
        let connections = self.server_connection_count(None);
        let watchers = self
            .server_watchers
            .iter_mut()
            .filter_map(|(&id, last)| {
                if *last == Some(connections) {
                    return None;
                }
                *last = Some(connections);
                Some(id)
            })
            .collect::<Vec<_>>();
        for id in watchers {
            self.send(
                id,
                ServerMessage::ServerStatus(ServerStatus {
                    instance_id: self.instance.clone(),
                    connections,
                    config_files: self.config_files.clone(),
                }),
            );
        }
    }

    pub(super) fn server_command(
        &mut self,
        id: u64,
        command: &Command,
        context: &ClientContext,
    ) -> Result<Outcome, String> {
        if command.args.len() != 1 {
            return Err("server management command takes no arguments".into());
        }
        if command.args[0] == b"tmuxx-server-status" {
            self.server_watchers.insert(id, None);
            return Ok(Outcome::default());
        }
        if !context.no_hooks
            && context
                .client_flags
                .split(',')
                .any(|flag| flag == "read-only")
        {
            return Err("client is read-only".into());
        }
        let connections = self.server_connection_count(Some(id));
        if connections != 0 {
            return Err(format!("server is busy: {connections} other connection(s)"));
        }
        self.effects(vec![Effect::Shutdown])?;
        Ok(Outcome::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        let (tx, _) = mpsc::sync_channel(64);
        Host::new(tx, &Options::new("unused-server-status"), BTreeMap::new())
    }

    fn client(host: &mut Host, id: u64) -> Receiver<ServerMessage> {
        let (tx, rx) = mpsc::sync_channel(64);
        host.clients.insert(
            id,
            Client {
                tx,
                context: ClientContext::default(),
                subscribed: false,
                control: false,
                delivery_bytes: Arc::new(AtomicUsize::new(0)),
            },
        );
        rx
    }

    fn request(host: &mut Host, id: u64, args: &[&str], subscribe: bool, control: bool) {
        host.request(
            id,
            Request {
                request_id: 1,
                argv: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin: None,
                subscribe,
                control,
            },
        );
    }

    fn drive(host: &mut Host) {
        for _ in 0..1024 {
            if !host.scheduled_work() {
                host.refresh_server_status();
                return;
            }
        }
        panic!("server management queue failed to quiesce");
    }

    fn statuses(rx: &Receiver<ServerMessage>) -> Vec<usize> {
        rx.try_iter()
            .filter_map(|message| match message {
                ServerMessage::ServerStatus(status) => Some(status.connections),
                ServerMessage::Snapshot { .. } | ServerMessage::Changed { .. } => {
                    panic!("management observer received terminal state")
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn status_reports_startup_paths_in_owner_directory_and_order() {
        let mut options = Options::new("unused-config-status");
        options.config = vec![
            PathBuf::from("config, first.conf"),
            std::env::temp_dir().join("配置.conf"),
        ];
        let (tx, _) = mpsc::sync_channel(64);
        let mut host = Host::new(tx, &options, BTreeMap::new());
        let observer = client(&mut host, 1);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        let status = observer
            .try_iter()
            .find_map(|message| match message {
                ServerMessage::ServerStatus(status) => Some(status),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            status.config_files,
            Some(vec![
                std::env::current_dir()
                    .unwrap()
                    .join("config, first.conf")
                    .into_os_string()
                    .into_string()
                    .unwrap(),
                options.config[1]
                    .clone()
                    .into_os_string()
                    .into_string()
                    .unwrap(),
            ])
        );
        assert_eq!(self::host().config_files, Some(vec![]));
    }

    #[test]
    fn source_reload_applies_files_in_order_keeps_sessions_and_reports_failures() {
        let directory = std::env::temp_dir().join(format!(
            "tmuxx-reload-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let paths = [
            directory.join("config, first.conf"),
            directory.join("配置;second.conf"),
        ];
        std::fs::write(&paths[0], "set -g @reload first\n").unwrap();
        std::fs::write(&paths[1], "set -g @reload startup\n").unwrap();
        let mut options = Options::new("unused-reload");
        options.config = paths.to_vec();
        let (tx, _) = mpsc::sync_channel(64);
        let mut host = Host::new(tx, &options, BTreeMap::new());
        host.start_configuration(&options.config);
        drive(&mut host);
        assert_eq!(host.engine.options["@reload"], "startup");
        host.engine
            .execute(
                &Command::new(vec![b"new-session".to_vec(), b"-d".to_vec()]),
                &mut ClientContext::default(),
            )
            .unwrap();
        let sessions = host.engine.sessions.keys().copied().collect::<Vec<_>>();
        let panes = host.engine.panes.keys().copied().collect::<Vec<_>>();
        let replies = client(&mut host, 1);
        // The remote client's directory must not affect the owner's absolute paths.
        host.clients.get_mut(&1).unwrap().context.cwd =
            Some(directory.join("missing").to_string_lossy().into());
        std::fs::write(&paths[1], "set -g @reload updated\n").unwrap();
        let files = host.config_files.clone().unwrap();
        request(
            &mut host,
            1,
            &["source-file", "--", &files[0], &files[1]],
            false,
            false,
        );
        drive(&mut host);
        assert_eq!(host.engine.options["@reload"], "updated");
        assert!(replies.try_iter().any(
            |message| matches!(message, ServerMessage::Reply(reply) if reply.error.is_none())
        ));
        assert_eq!(
            host.engine.sessions.keys().copied().collect::<Vec<_>>(),
            sessions
        );
        assert_eq!(host.engine.panes.keys().copied().collect::<Vec<_>>(), panes);
        std::fs::write(&paths[1], "set -g @reload \"unterminated\n").unwrap();
        request(
            &mut host,
            1,
            &["source-file", "--", &files[1]],
            false,
            false,
        );
        drive(&mut host);
        assert!(replies.try_iter().any(|message| matches!(message, ServerMessage::Reply(reply)
            if reply.status == Some(1) && String::from_utf8_lossy(&reply.output).contains("unterminated"))));
        request(&mut host, 1, &["source-file", "--", &files[1]], true, false);
        drive(&mut host);
        assert!(replies.try_iter().any(|message| matches!(message, ServerMessage::Reply(reply)
            if reply.status == Some(1) && String::from_utf8_lossy(&reply.output).contains("unterminated"))));
        std::fs::remove_file(&paths[1]).unwrap();
        request(
            &mut host,
            1,
            &["source-file", "--", &files[1]],
            false,
            false,
        );
        drive(&mut host);
        assert!(replies.try_iter().any(
            |message| matches!(message, ServerMessage::Reply(reply) if reply.error.is_some())
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn status_is_opt_in_deduplicated_and_excludes_observers_and_internal_clients() {
        let mut host = host();
        let observer = client(&mut host, 1);
        let second = client(&mut host, 2);
        let regular = client(&mut host, 3);
        host.refresh_server_status();
        assert!(observer.try_recv().is_err());
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        assert_eq!(statuses(&observer), [2]);
        assert!(regular.try_recv().is_err());
        request(&mut host, 2, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        assert_eq!(statuses(&observer), [1]);
        assert_eq!(statuses(&second), [1]);
        host.refresh_server_status();
        assert!(observer.try_recv().is_err());
        host.internal_queue(
            ClientContext::default(),
            vec![Command::new(vec![
                b"wait-for".to_vec(),
                b"pending".to_vec(),
            ])],
            0,
            false,
        );
        assert_eq!(host.server_connection_count(None), 1);
        host.disconnect(3);
        host.refresh_server_status();
        assert_eq!(statuses(&observer), [0]);
        assert_eq!(statuses(&second), [0]);
    }

    #[test]
    fn idle_stop_checks_connections_arriving_after_idle_status() {
        let mut host = host();
        let observer = client(&mut host, 1);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        assert_eq!(statuses(&observer), [0]);
        let _new_client = client(&mut host, 2);
        request(&mut host, 1, &["tmuxx-stop-if-idle"], false, false);
        drive(&mut host);
        assert!(!host.stopping);
        assert!(observer.try_iter().any(|message| matches!(
            message,
            ServerMessage::Reply(Response { error: Some(error), .. })
                if error == "server is busy: 1 other connection(s)"
        )));
        host.disconnect(2);
        let _second_observer = client(&mut host, 3);
        request(&mut host, 3, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        request(&mut host, 1, &["tmuxx-stop-if-idle"], false, false);
        drive(&mut host);
        assert!(host.stopping);
        assert!(
            observer.try_iter().any(|message| matches!(
                message,
                ServerMessage::Reply(Response { error: None, .. })
            ))
        );
    }

    #[test]
    fn queued_work_revokes_management_role_before_watch_runs() {
        let mut host = host();
        let observer = client(&mut host, 1);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        request(
            &mut host,
            1,
            &["display-message", "-p", "work"],
            false,
            false,
        );
        assert!(!host.is_management_client(1));
        drive(&mut host);
        assert_eq!(statuses(&observer), [1]);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        assert_eq!(statuses(&observer), [1]);
        assert!(!host.is_management_client(1));
    }

    #[test]
    fn subscribed_and_control_work_connections_stay_counted() {
        let mut host = host();
        let _subscribed = client(&mut host, 1);
        let _control = client(&mut host, 2);
        request(&mut host, 1, &["list-sessions"], true, false);
        request(
            &mut host,
            2,
            &["display-message", "-p", "size or copy"],
            false,
            true,
        );
        drive(&mut host);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        request(&mut host, 2, &["tmuxx-server-status"], false, true);
        drive(&mut host);
        assert_eq!(host.server_connection_count(None), 2);
    }

    #[test]
    fn status_delivery_uses_bounded_control_queue() {
        let mut host = host();
        let observer = client(&mut host, 1);
        request(&mut host, 1, &["tmuxx-server-status"], false, true);
        drive(&mut host);
        assert_eq!(statuses(&observer), [0]);
        host.clients[&1]
            .delivery_bytes
            .store(64 * 1024 * 1024, Ordering::Release);
        let _other = client(&mut host, 2);
        host.refresh_server_status();
        assert!(!host.clients.contains_key(&1));
        assert!(!host.server_watchers.contains_key(&1));
    }

    #[test]
    fn management_observer_does_not_prevent_exit_empty() {
        let (tx, events) = mpsc::sync_channel(64);
        let mut options = Options::new("unused-server-status");
        options.exit_empty = true;
        let mut host = Host::new(tx.clone(), &options, BTreeMap::new());
        host.engine
            .execute(
                &Command::new(vec![
                    b"new-session".to_vec(),
                    b"-d".to_vec(),
                    b"-s".to_vec(),
                    b"test".to_vec(),
                ]),
                &mut ClientContext::default(),
            )
            .unwrap();
        let _observer = client(&mut host, 1);
        let _regular = client(&mut host, 2);
        request(&mut host, 1, &["tmuxx-server-status"], false, false);
        drive(&mut host);
        tx.send(Event::Request {
            id: 2,
            request: Request {
                request_id: 1,
                argv: vec![b"kill-session".to_vec(), b"-t".to_vec(), b"test".to_vec()],
                stdin: None,
                subscribe: false,
                control: false,
            },
        })
        .unwrap();
        tx.send(Event::Disconnect(2)).unwrap();
        // A broken empty-server guard reaches this fallback instead of exiting.
        tx.send(Event::Shutdown).unwrap();
        host.run(events, &options).unwrap();
        assert!(host.engine.sessions.is_empty());
        assert!(
            !host.stopping,
            "management observer prevented automatic exit"
        );
    }
}
