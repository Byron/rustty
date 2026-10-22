//! Opt-in visual-state copying between macOS instances, using signals and private files.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const PROTOCOL: u32 = 1;
const SIGNAL: libc::c_int = libc::SIGURG;
const MAX_METADATA: u64 = 64 * 1024;
pub const MAX_DUMP_BYTES: u64 = 512 * 1024 * 1024;
const DUMP_EXTENSION: &str = "rustty-state.lz4";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub pid: u32,
    pub label: String,
    pub executable: PathBuf,
    pub started_at: u64,
    started_micros: u64,
    protocol: u32,
    #[serde(default)]
    pub ignoring_requests: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub requester: Instance,
    pub source: Instance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestStatus {
    Waiting,
    Ready { path: PathBuf, bytes: u64 },
    Declined,
    Ignored,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    #[default]
    Waiting,
    Declined,
    Ignored,
}

#[derive(Serialize, Deserialize)]
struct RequestFile {
    #[serde(flatten)]
    request: Request,
    #[serde(default)]
    reply: Reply,
}

pub struct Registry {
    directory: PathBuf,
    instance: Instance,
    prompted: HashSet<String>,
    _watcher: Option<SignalWatcher>,
}

impl Registry {
    pub fn register(label: String, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let directory = user_temporary_directory()?.join("rustty-handoff");
        Self::register_with_watcher(directory, label, wake)
    }

    #[cfg(debug_assertions)]
    pub fn register_isolated(
        directory: PathBuf,
        label: String,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        Self::register_with_watcher(directory, label, wake)
    }

    fn register_with_watcher(
        directory: PathBuf,
        label: String,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        private_directory(&directory)?;
        // Publish support only after a native signal listener is ready.
        let watcher = SignalWatcher::new(wake)?;
        let registry = Self::register_in(directory, label, Some(watcher))?;
        registry.clean_stale()?;
        Ok(registry)
    }

    fn register_in(
        directory: PathBuf,
        label: String,
        watcher: Option<SignalWatcher>,
    ) -> io::Result<Self> {
        private_directory(&directory)?;
        let pid = std::process::id();
        let (started_at, started_micros) = process_identity(pid)?;
        let instance = Instance {
            id: format!("{pid}-{started_at}-{started_micros}-{}", unique_id()),
            pid,
            label,
            executable: std::env::current_exe()?,
            started_at,
            started_micros,
            protocol: PROTOCOL,
            ignoring_requests: false,
        };
        let registry = Self {
            directory,
            instance,
            prompted: HashSet::new(),
            _watcher: watcher,
        };
        private_directory(&registry.request_directory(&registry.instance))?;
        write_json(
            &registry.instance_path(&registry.instance),
            &registry.instance,
        )?;
        Ok(registry)
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// Normal launches and successful imports explicitly select the canonical saver.
    /// Ownership survives exit so a still-running older instance cannot reclaim it.
    pub fn claim_workspace(&self, canonical: &Path) -> io::Result<()> {
        let owner = workspace_owner_path(canonical)?;
        let mut output = gix_lock::File::acquire_to_update_resource(
            owner,
            gix_lock::acquire::Fail::AfterDurationWithBackoff(Duration::from_secs(1)),
            None,
            0,
        )
        .map_err(io::Error::other)?;
        output.with_mut(|file| file.set_permissions(fs::Permissions::from_mode(0o600)))?;
        serde_json::to_writer(&mut output, &self.instance.id).map_err(io::Error::other)?;
        output.commit().map_err(io::Error::other)?;
        Ok(())
    }

    pub fn save_workspace(
        &self,
        canonical: &Path,
        workspace: &crate::workspace::Workspace,
    ) -> io::Result<()> {
        let owner = workspace_owner_path(canonical)?;
        let _lock = gix_lock::Marker::acquire_to_hold_resource(
            &owner,
            gix_lock::acquire::Fail::AfterDurationWithBackoff(Duration::from_secs(1)),
            None,
            0,
        )
        .map_err(io::Error::other)?;
        let owns_workspace = match read_json::<String>(&owner) {
            Ok(id) => id == self.instance.id,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if owns_workspace {
            workspace.save(canonical)
        } else {
            workspace
                .save(&canonical.with_file_name(format!("workspace-{}.json", self.instance.id)))
        }
    }

    pub fn instances(&self) -> io::Result<Vec<Instance>> {
        let mut instances = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "instance")
                && let Ok(instance) = read_json::<Instance>(&path)
                && instance.id != self.instance.id
                && path == self.instance_path(&instance)
                && self.is_live(&instance)
            {
                instances.push(instance);
            }
        }
        instances.sort_by_key(|instance| (instance.started_at, instance.pid));
        Ok(instances)
    }

    pub fn ask(&self, source: &Instance) -> io::Result<Request> {
        if !self.is_live(source) || source.id == self.instance.id {
            return Err(io::Error::other(
                "This Rustty instance is no longer available",
            ));
        }
        if source.ignoring_requests {
            return Err(io::Error::other(
                "This Rustty instance is ignoring requests",
            ));
        }
        let request = Request {
            id: unique_id(),
            requester: self.instance.clone(),
            source: source.clone(),
        };
        write_json(
            &self.request_path(&request),
            &RequestFile {
                request: request.clone(),
                reply: Reply::Waiting,
            },
        )?;
        if let Err(error) = self.notify(source) {
            let _ = self.cancel(&request);
            return Err(error);
        }
        Ok(request)
    }

    /// Returns each pending confirmation once; cancelled or dead requesters are skipped.
    pub fn incoming(&mut self) -> io::Result<Vec<Request>> {
        let mut incoming = Vec::new();
        for requester in self.instances()? {
            let directory = self.request_directory(&requester);
            if check_private_directory(&directory).is_err() {
                continue;
            }
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "request")
                {
                    continue;
                }
                let Ok(file) = read_json::<RequestFile>(&path) else {
                    continue;
                };
                let request = file.request;
                if !valid_request(&request)
                    || request.source.id != self.instance.id
                    || request.requester.id != requester.id
                    || path != self.request_path(&request)
                    || !matches!(file.reply, Reply::Waiting)
                    || self.dump_path(&request).exists()
                {
                    continue;
                }
                if self.instance.ignoring_requests {
                    self.reply(&request, Reply::Ignored)?;
                } else if self
                    .prompted
                    .insert(format!("{}/{}", requester.id, request.id))
                {
                    incoming.push(request);
                }
            }
        }
        Ok(incoming)
    }

    pub fn ignore_requests(&mut self, ignore: bool) -> io::Result<()> {
        self.instance.ignoring_requests = ignore;
        write_json(&self.instance_path(&self.instance), &self.instance)?;
        if ignore {
            self.incoming()?;
        }
        Ok(())
    }

    /// Called only after the source user approves; None records a declined request.
    pub fn respond(&self, request: &Request, payload: Option<&[u8]>) -> io::Result<()> {
        self.check_source(request)?;
        let Some(payload) = payload else {
            return self.reply(request, Reply::Declined);
        };
        if payload.len() as u64 > MAX_DUMP_BYTES {
            return Err(io::Error::other(
                "Visual state exceeds the 512 MiB export limit",
            ));
        }
        if self.pending_file(request)?.is_none() {
            return Ok(());
        }
        // Compress outside the short lifecycle lock so cancellation remains responsive.
        let mut output = lock_file(&self.dump_path(request))?;
        {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut output);
            encoder.write_all(payload)?;
            encoder.finish().map_err(io::Error::other)?;
        }
        output.with_mut(|file| file.sync_all())?;
        if fs::metadata(output.lock_path())?.len() > MAX_DUMP_BYTES {
            return Err(io::Error::other(
                "Compressed visual state exceeds the export limit",
            ));
        }
        let _lifecycle = self.lifecycle_lock(request)?;
        if self.pending_file(request)?.is_none() {
            return Ok(());
        }
        output.commit().map_err(io::Error::other)?;
        let _ = self.notify(&request.requester);
        Ok(())
    }

    pub fn is_pending(&self, request: &Request) -> io::Result<bool> {
        self.check_source(request)?;
        Ok(self.pending_file(request)?.is_some() && !self.dump_path(request).exists())
    }

    pub fn status(&self, request: &Request) -> io::Result<RequestStatus> {
        self.check_requester(request)?;
        let file = match read_json::<RequestFile>(&self.request_path(request)) {
            Ok(file) if file.request == *request => file,
            Ok(_) => return Err(io::Error::other("Visual-state request identity changed")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RequestStatus::Unavailable);
            }
            Err(error) => return Err(error),
        };
        let path = self.dump_path(request);
        match open_private_file(&path, MAX_DUMP_BYTES) {
            Ok(file) => {
                return Ok(RequestStatus::Ready {
                    path,
                    bytes: file.metadata()?.len(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(match file.reply {
            Reply::Declined => RequestStatus::Declined,
            Reply::Ignored => RequestStatus::Ignored,
            Reply::Waiting if self.is_live(&request.source) => RequestStatus::Waiting,
            Reply::Waiting => RequestStatus::Unavailable,
        })
    }

    /// Leaves files intact until the caller has successfully imported the decoded state.
    pub fn read_dump(&self, request: &Request) -> io::Result<Vec<u8>> {
        self.check_requester(request)?;
        let _lifecycle = self.lifecycle_lock(request)?;
        let file = read_json::<RequestFile>(&self.request_path(request))?;
        if file.request != *request {
            return Err(io::Error::other("Visual-state request identity changed"));
        }
        let input = open_private_file(&self.dump_path(request), MAX_DUMP_BYTES)?;
        decode_dump(input)
    }

    pub fn cancel(&self, request: &Request) -> io::Result<()> {
        self.check_requester(request)?;
        let _lifecycle = self.lifecycle_lock(request)?;
        remove_if_present(&self.request_path(request))?;
        remove_if_present(&self.dump_path(request))?;
        let _ = self.notify(&request.source);
        Ok(())
    }

    pub fn cancel_all(&self) -> io::Result<()> {
        let directory = self.request_directory(&self.instance);
        let sources: Vec<_> = fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "request")
            })
            .filter_map(|entry| read_json::<RequestFile>(&entry.path()).ok())
            .filter(|file| {
                valid_request(&file.request) && file.request.requester.id == self.instance.id
            })
            .map(|file| file.request.source)
            .collect();
        // Renaming invalidates even a writer holding an open staging file. Its final
        // request check/commit uses the old paths, so a cancelled dump cannot reappear.
        let cancelled =
            self.directory
                .join(format!("{}-{}.cancelled", self.instance.id, unique_id()));
        match fs::rename(&directory, &cancelled) {
            Ok(()) => {
                private_directory(&directory)?;
                fs::remove_dir_all(cancelled)?;
                let mut notified = HashSet::new();
                for source in sources {
                    if notified.insert(source.id.clone()) {
                        let _ = self.notify(&source);
                    }
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn dump_path(&self, request: &Request) -> PathBuf {
        self.request_directory(&request.requester)
            .join(format!("{}.{}", request.id, DUMP_EXTENSION))
    }

    fn request_path(&self, request: &Request) -> PathBuf {
        self.request_directory(&request.requester)
            .join(format!("{}.request", request.id))
    }

    fn request_directory(&self, instance: &Instance) -> PathBuf {
        self.directory.join(format!("{}.requests", instance.id))
    }

    fn instance_path(&self, instance: &Instance) -> PathBuf {
        self.directory.join(format!("{}.instance", instance.id))
    }

    fn lifecycle_lock(&self, request: &Request) -> io::Result<gix_lock::Marker> {
        let directory = self.request_directory(&request.requester);
        check_private_directory(&directory)?;
        let path = directory.join(format!("{}.lifecycle", request.id));
        gix_lock::Marker::acquire_to_hold_resource(
            path,
            gix_lock::acquire::Fail::AfterDurationWithBackoff(Duration::from_secs(1)),
            None,
            0,
        )
        .map_err(io::Error::other)
    }

    fn check_source(&self, request: &Request) -> io::Result<()> {
        if !valid_request(request) || request.source.id != self.instance.id {
            return Err(io::Error::other(
                "This visual-state request belongs to another source",
            ));
        }
        Ok(())
    }

    fn check_requester(&self, request: &Request) -> io::Result<()> {
        if !valid_request(request) || request.requester.id != self.instance.id {
            return Err(io::Error::other(
                "This visual-state request belongs to another requester",
            ));
        }
        Ok(())
    }

    fn pending_file(&self, request: &Request) -> io::Result<Option<RequestFile>> {
        if !self.is_live(&request.requester) {
            return Ok(None);
        }
        match read_json::<RequestFile>(&self.request_path(request)) {
            Ok(file) if file.request == *request && matches!(file.reply, Reply::Waiting) => {
                Ok(Some(file))
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn reply(&self, request: &Request, reply: Reply) -> io::Result<()> {
        self.check_source(request)?;
        if let Some(mut file) = self.pending_file(request)? {
            file.reply = reply;
            let mut output = lock_file(&self.request_path(request))?;
            serde_json::to_writer(&mut output, &file).map_err(io::Error::other)?;
            let _lifecycle = self.lifecycle_lock(request)?;
            if self.pending_file(request)?.is_none() {
                return Ok(());
            }
            output.commit().map_err(io::Error::other)?;
            let _ = self.notify(&request.requester);
        }
        Ok(())
    }

    fn is_live(&self, instance: &Instance) -> bool {
        valid_instance(instance)
            && process_identity(instance.pid)
                .is_ok_and(|identity| identity == (instance.started_at, instance.started_micros))
            && read_json::<Instance>(&self.instance_path(instance)).is_ok_and(|current| {
                current.id == instance.id
                    && current.pid == instance.pid
                    && current.protocol == PROTOCOL
                    && current.started_at == instance.started_at
                    && current.started_micros == instance.started_micros
            })
    }

    fn notify(&self, instance: &Instance) -> io::Result<()> {
        if !self.is_live(instance) {
            return Err(io::Error::other(
                "This Rustty instance is no longer available",
            ));
        }
        #[cfg(test)]
        if self._watcher.is_none() {
            return Ok(());
        }
        // SIGURG is ignored by default: the unavoidable PID-check/kill race must
        // never terminate an unrelated process that happens to reuse the PID.
        if unsafe { libc::kill(instance.pid as libc::pid_t, SIGNAL) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn clean_stale(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(id) = [".instance", ".instance.lock", ".requests", ".cancelled"]
                .into_iter()
                .find_map(|suffix| name.strip_suffix(suffix))
            else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            let mut components = id.splitn(4, '-');
            let identity = components.next().and_then(|pid| pid.parse().ok()).zip(
                components
                    .next()
                    .and_then(|seconds| seconds.parse().ok())
                    .zip(components.next().and_then(|micros| micros.parse().ok())),
            );
            let Some((pid, identity)) = identity else {
                continue;
            };
            if process_identity(pid).is_ok_and(|current| current == identity) {
                continue;
            }
            if check_private_directory(&path).is_ok() {
                fs::remove_dir_all(path)?;
            } else {
                remove_if_present(&path)?;
            }
        }
        Ok(())
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        let _ = self.cancel_all();
        let _ = fs::remove_dir(self.request_directory(&self.instance));
        let _ = remove_if_present(&self.instance_path(&self.instance));
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

fn workspace_owner_path(canonical: &Path) -> io::Result<PathBuf> {
    let parent = canonical
        .parent()
        .ok_or_else(|| io::Error::other("Workspace path has no parent"))?;
    let name = canonical
        .file_name()
        .ok_or_else(|| io::Error::other("Workspace path has no filename"))?;
    fs::create_dir_all(parent)?;
    let directory = parent.join(".workspace-owner");
    private_directory(&directory)?;
    Ok(directory.join(name))
}

fn valid_instance(instance: &Instance) -> bool {
    valid_id(&instance.id)
        && instance.pid > 1
        && instance.pid <= i32::MAX as u32
        && instance.protocol == PROTOCOL
        && instance.label.len() <= 4096
}

fn valid_request(request: &Request) -> bool {
    valid_id(&request.id) && valid_instance(&request.source) && valid_instance(&request.requester)
}

fn unique_id() -> String {
    let mut bytes = [0u8; 16];
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn user_temporary_directory() -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let size = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if size == 0 || size > 1024 * 1024 {
        return Err(io::Error::other(
            "Cannot locate the macOS user temporary directory",
        ));
    }
    let mut bytes = vec![0; size];
    if unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            bytes.as_mut_ptr().cast(),
            size,
        )
    } != size
    {
        return Err(io::Error::other(
            "The macOS user temporary directory changed",
        ));
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn process_identity(pid: u32) -> io::Result<(u64, u64)> {
    if pid <= 1 || pid > i32::MAX as u32 {
        return Err(io::Error::other("Invalid Rustty process identifier"));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return Err(io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Rustty belongs to another user",
        ));
    }
    Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

fn private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    check_private_directory(path)
}

fn check_private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unsafe visual-state directory",
        ));
    }
    Ok(())
}

fn open_private_file(path: &Path, limit: u64) -> io::Result<File> {
    check_private_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > limit
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsafe or oversized visual-state file",
        ));
    }
    Ok(file)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let file = open_private_file(path, MAX_METADATA)?;
    serde_json::from_reader(file.take(MAX_METADATA + 1)).map_err(io::Error::other)
}

fn lock_file(path: &Path) -> io::Result<gix_lock::File> {
    check_private_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?;
    let mut output = gix_lock::File::acquire_to_update_resource(
        path,
        gix_lock::acquire::Fail::Immediately,
        None,
        0,
    )
    .map_err(io::Error::other)?;
    output.with_mut(|file| file.set_permissions(fs::Permissions::from_mode(0o600)))?;
    Ok(output)
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut output = lock_file(path)?;
    serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
    output.commit().map_err(io::Error::other)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn decode_dump(input: impl Read) -> io::Result<Vec<u8>> {
    decode_dump_with_limit(input, MAX_DUMP_BYTES)
}

fn decode_dump_with_limit(input: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    lz4_flex::frame::FrameDecoder::new(input)
        .take(limit + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Decompressed visual state exceeds the import limit",
        ));
    }
    Ok(output)
}

struct SignalWatcher {
    queue: Arc<OwnedFd>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SignalWatcher {
    fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        static SIGNAL_INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
        let installed = SIGNAL_INSTALLED.get_or_init(|| {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = signal_handler as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) } != 0 {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(())
            }
        });
        if let Err(error) = installed {
            return Err(io::Error::other(error.clone()));
        }
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let changes = [
            event(
                SIGNAL as usize,
                libc::EVFILT_SIGNAL,
                libc::EV_ADD | libc::EV_CLEAR,
                0,
            ),
            event(1, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0),
        ];
        if unsafe {
            libc::kevent(
                raw,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_queue = Arc::clone(&queue);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("rustty-handoff-signal".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let mut received = event(0, 0, 0, 0);
                    let count = unsafe {
                        libc::kevent(
                            thread_queue.as_raw_fd(),
                            std::ptr::null(),
                            0,
                            &mut received,
                            1,
                            std::ptr::null(),
                        )
                    };
                    if count < 0 {
                        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if !thread_stop.load(Ordering::Acquire)
                        && received.filter == libc::EVFILT_SIGNAL
                    {
                        wake();
                    }
                }
            })?;
        Ok(Self {
            queue,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let trigger = event(1, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &trigger,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

extern "C" fn signal_handler(_: libc::c_int) {}

fn event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rustty-handoff-test-{}", unique_id()));
            private_directory(&path).unwrap();
            Self(path)
        }

        fn registry(&self, label: &str) -> Registry {
            Registry::register_in(self.0.clone(), label.into(), None).unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn request(source: &Registry, requester: &Registry) -> Request {
        // Test file lifecycle without sending any process a signal.
        let request = Request {
            id: unique_id(),
            requester: requester.instance.clone(),
            source: source.instance.clone(),
        };
        write_json(
            &requester.request_path(&request),
            &RequestFile {
                request: request.clone(),
                reply: Reply::Waiting,
            },
        )
        .unwrap();
        request
    }

    #[test]
    fn compressed_export_retains_data_until_import_finishes() {
        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let requester = directory.registry("requester");
        let request = request(&source, &requester);
        let state = "scrollback and alternate screen\n".repeat(1000);
        source.respond(&request, Some(state.as_bytes())).unwrap();
        assert!(
            matches!(requester.status(&request).unwrap(), RequestStatus::Ready { bytes, .. } if bytes < state.len() as u64)
        );
        assert_eq!(requester.read_dump(&request).unwrap(), state.as_bytes());
        assert!(requester.dump_path(&request).exists());
        requester.cancel_all().unwrap();
        assert!(!requester.dump_path(&request).exists());
        assert!(!requester.request_path(&request).exists());
    }

    #[test]
    fn cancellation_prevents_late_response_and_does_not_touch_other_requesters() {
        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let requester = directory.registry("requester");
        let other = directory.registry("other");
        let cancelled = request(&source, &requester);
        let retained = request(&source, &other);
        requester.cancel(&cancelled).unwrap();
        source.respond(&cancelled, Some(b"late")).unwrap();
        assert!(!requester.dump_path(&cancelled).exists());
        source.respond(&retained, Some(b"keep")).unwrap();
        requester.cancel_all().unwrap();
        assert_eq!(other.read_dump(&retained).unwrap(), b"keep");
    }

    #[test]
    fn cancelling_all_invalidates_an_already_open_staging_file() {
        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let requester = directory.registry("requester");
        let request = request(&source, &requester);
        let mut output = lock_file(&source.dump_path(&request)).unwrap();
        output.write_all(b"still writing").unwrap();
        requester.cancel_all().unwrap();
        assert!(source.pending_file(&request).unwrap().is_none());
        assert!(output.commit().is_err());
        assert!(!requester.dump_path(&request).exists());
    }

    #[test]
    fn source_can_ignore_pending_and_future_requests() {
        let directory = TestDirectory::new();
        let mut source = directory.registry("source");
        let requester = directory.registry("requester");
        let first = request(&source, &requester);
        assert_eq!(source.incoming().unwrap(), vec![first.clone()]);
        assert!(source.incoming().unwrap().is_empty());
        source.ignore_requests(true).unwrap();
        assert_eq!(requester.status(&first).unwrap(), RequestStatus::Ignored);
        let second = request(&source, &requester);
        assert!(source.incoming().unwrap().is_empty());
        assert_eq!(requester.status(&second).unwrap(), RequestStatus::Ignored);
    }

    #[test]
    fn discovery_rejects_pid_reuse_and_unsupported_protocols() {
        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let requester = directory.registry("requester");
        assert_eq!(requester.instances().unwrap().len(), 1);
        let mut stale = source.instance.clone();
        stale.started_micros += 1;
        write_json(&source.instance_path(&stale), &stale).unwrap();
        assert!(requester.instances().unwrap().is_empty());
        stale = source.instance.clone();
        stale.protocol += 1;
        write_json(&source.instance_path(&stale), &stale).unwrap();
        requester.clean_stale().unwrap();
        assert!(source.instance_path(&stale).exists());
        assert!(requester.instances().unwrap().is_empty());
    }

    #[test]
    fn stale_requester_files_are_removed_on_cleanup() {
        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let requester = directory.registry("requester");
        let mut stale = requester.instance.clone();
        stale.started_at = 0;
        stale.started_micros = 0;
        stale.id = format!("{}-0-0-{}", stale.pid, unique_id());
        write_json(&source.instance_path(&stale), &stale).unwrap();
        private_directory(&source.request_directory(&stale)).unwrap();
        fs::write(source.request_directory(&stale).join("dump"), b"old state").unwrap();
        source.clean_stale().unwrap();
        assert!(!source.instance_path(&stale).exists());
        assert!(!source.request_directory(&stale).exists());
        assert!(source.instance_path(requester.instance()).exists());
    }

    #[test]
    fn native_signal_wakes_and_stops_the_kqueue_worker() {
        let (send, receive) = std::sync::mpsc::channel();
        let watcher = SignalWatcher::new(Arc::new(move || {
            let _ = send.send(());
        }))
        .unwrap();
        // This disposable test process is the only signal target.
        assert_eq!(unsafe { libc::kill(std::process::id() as i32, SIGNAL) }, 0);
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(watcher);
    }

    #[test]
    fn older_instances_cannot_overwrite_a_claimed_workspace_even_after_owner_exit() {
        use crate::workspace::{Tab, WindowState, Workspace};

        fn workspace(position: f64) -> Workspace {
            let mut workspace = Workspace::default();
            let window = workspace.id();
            let tab = workspace.id();
            let pane = workspace.id();
            workspace.windows.push(WindowState {
                id: window,
                tabs: vec![Tab::new(tab, pane, PathBuf::from("/tmp"))],
                active_tab: 0,
                frame: [position, 0., 800., 600.],
                quick: false,
            });
            workspace
        }

        let directory = TestDirectory::new();
        let source = directory.registry("source");
        let recipient = directory.registry("recipient");
        let canonical = directory.0.join("data/workspace.json");
        let backup = canonical.with_file_name(format!("workspace-{}.json", source.instance.id));

        source.save_workspace(&canonical, &workspace(100.)).unwrap();
        assert!(!canonical.exists());
        assert!(backup.exists());
        source.claim_workspace(&canonical).unwrap();
        source.save_workspace(&canonical, &workspace(100.)).unwrap();
        assert_eq!(
            Workspace::load(&canonical).unwrap().unwrap().windows[0].frame[0],
            100.
        );

        recipient.claim_workspace(&canonical).unwrap();
        recipient
            .save_workspace(&canonical, &workspace(200.))
            .unwrap();
        source.save_workspace(&canonical, &workspace(300.)).unwrap();
        assert_eq!(
            Workspace::load(&canonical).unwrap().unwrap().windows[0].frame[0],
            200.
        );
        assert_eq!(
            Workspace::load(&backup).unwrap().unwrap().windows[0].frame[0],
            300.
        );

        drop(recipient);
        source.save_workspace(&canonical, &workspace(400.)).unwrap();
        assert_eq!(
            Workspace::load(&canonical).unwrap().unwrap().windows[0].frame[0],
            200.
        );
        assert_eq!(
            Workspace::load(&backup).unwrap().unwrap().windows[0].frame[0],
            400.
        );
    }

    #[test]
    fn rejects_symlinks_and_non_private_files_and_bounds_decompression() {
        let directory = TestDirectory::new();
        let path = directory.0.join("payload");
        fs::write(&path, b"secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_private_file(&path, 100).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.0.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(open_private_file(&link, 100).is_err());
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        encoder.write_all(&[0; 1024]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(decode_dump_with_limit(compressed.as_slice(), 100).is_err());
        assert!(decode_dump(b"corrupt data".as_slice()).is_err());
    }
}
