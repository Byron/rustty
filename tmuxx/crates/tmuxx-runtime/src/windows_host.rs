use super::*;
use windows::{Listener, PipeStream};

struct AcceptGuard {
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    connections: Arc<Mutex<BTreeMap<u64, PipeStream>>>,
}

impl Drop for AcceptGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        for stream in self.connections.lock().unwrap().values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

struct ConnectionGuard {
    id: u64,
    connections: Arc<Mutex<BTreeMap<u64, PipeStream>>>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.lock().unwrap().remove(&self.id);
    }
}

pub(super) fn start(options: Options) -> io::Result<ServerHandle> {
    let default_child_path = defaults::native_search_path()?;
    if options.tmux_endpoint.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows named pipes do not implement Unix tmux IPC",
        ));
    }
    for path in options
        .config
        .iter()
        .chain(std::iter::once(&options.endpoint))
    {
        if path.to_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native Windows paths must be Unicode",
            ));
        }
    }
    // Fail before exposing an owner if the shipped ConPTY runtime is unavailable.
    let executable = std::env::current_exe()?;
    let resources = std::env::var_os("TMUXX_CONPTY_RESOURCES")
        .map(PathBuf::from)
        .unwrap_or(
            executable
                .parent()
                .ok_or_else(|| io::Error::other("executable has no resource directory"))?
                .join("resources"),
        );
    super::initialize_conpty(&resources)?;
    let environment = host_environment(std::env::vars_os())?;
    let mut listener = Listener::bind(&options.endpoint)?;
    let endpoint = options.endpoint.clone();
    let (tx, rx) = mpsc::sync_channel(1024);
    let mut host = Host::new(tx.clone(), &options, environment);
    host.engine.default_child_path = default_child_path;
    let instance = host.instance.clone();
    let next_client = Arc::new(AtomicU64::new(0));
    let accept_ids = next_client.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let accept_stop = stopped.clone();
    let accept_tx = tx.clone();
    let connections = Arc::new(Mutex::new(BTreeMap::new()));
    let accepted = connections.clone();
    let accept_thread = thread::Builder::new()
        .name("tmuxx-accept".into())
        .spawn(move || {
            while !accept_stop.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok(Some(stream)) => stream,
                    Ok(None) => continue,
                    Err(error) => {
                        eprintln!("tmuxx: named-pipe accept: {error}");
                        break;
                    }
                };
                let id = accept_ids.fetch_add(1, Ordering::Relaxed);
                let guard = {
                    let mut clients = accepted.lock().unwrap();
                    if clients.len() >= MAX_SOCKET_CLIENTS {
                        continue;
                    }
                    clients.insert(id, stream.clone());
                    ConnectionGuard {
                        id,
                        connections: accepted.clone(),
                    }
                };
                let tx = accept_tx.clone();
                let instance = instance.clone();
                let _ = thread::Builder::new()
                    .name(format!("tmuxx-client-{id}"))
                    .spawn(move || {
                        let _guard = guard;
                        if let Err(error) = serve_connection(stream, id, tx, Some(instance)) {
                            eprintln!("tmuxx: connection {id}: {error}");
                        }
                    });
            }
        })?;
    let accept = AcceptGuard {
        stopped,
        thread: Some(accept_thread),
        connections,
    };
    let engine = thread::Builder::new()
        .name("tmuxx-owner".into())
        .spawn(move || {
            let _accept = accept;
            let result = host.run(rx, &options);
            host.scheduler.shutdown();
            host.processes.clear();
            for client in host.clients.values() {
                let _ = client.tx.try_send(ServerMessage::Exit {
                    reason: "server exited".into(),
                });
            }
            host.clients.clear();
            result
        })?;
    Ok(ServerHandle {
        tx,
        next_client,
        thread: Some(engine),
        endpoint,
    })
}
