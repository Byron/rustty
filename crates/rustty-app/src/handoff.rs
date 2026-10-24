//! Persistent session dumps in the current user's private temporary directory.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "macos")]
#[path = "handoff/macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "handoff/windows.rs"]
mod platform;
use platform::{
    check_private_directory, open_private_file, private_directory, protect_file, unique_id,
    user_temporary_directory,
};

const MAX_METADATA: u64 = 64 * 1024;
pub const MAX_DUMP_BYTES: u64 = 512 * 1024 * 1024;
const DUMP_EXTENSION: &str = "rustty-state.lz4";
const METADATA_EXTENSION: &str = "session.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDump {
    pub id: String,
    pub label: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: u64,
    /// Compressed size on disk.
    pub bytes: u64,
}

pub struct Store {
    directory: PathBuf,
    instance_id: String,
    label: String,
}

impl Store {
    pub fn open(label: String) -> io::Result<Self> {
        Self::open_in(
            user_temporary_directory()?.join("rustty-session-dumps"),
            label,
        )
    }

    #[cfg(debug_assertions)]
    pub fn open_isolated(directory: PathBuf, label: String) -> io::Result<Self> {
        Self::open_in(directory, label)
    }

    fn open_in(directory: PathBuf, label: String) -> io::Result<Self> {
        if label.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Session label is too long",
            ));
        }
        private_directory(&directory)?;
        Ok(Self {
            directory,
            instance_id: unique_id(),
            label,
        })
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
        output.with_mut(|file| protect_file(file))?;
        serde_json::to_writer(&mut output, &self.instance_id).map_err(io::Error::other)?;
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
            Ok(id) => id == self.instance_id,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if owns_workspace {
            workspace.save(canonical)
        } else {
            workspace
                .save(&canonical.with_file_name(format!("workspace-{}.json", self.instance_id)))
        }
    }

    /// Only complete, private dumps are offered, with the newest first.
    pub fn dumps(&self) -> io::Result<Vec<SavedDump>> {
        check_private_directory(&self.directory)?;
        let mut dumps = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".session.json"))
                && let Ok(dump) = read_json::<SavedDump>(&path)
                && valid_dump(&dump)
                && path == self.metadata_path(&dump)
                && self.open_dump(&dump).is_ok()
            {
                dumps.push(dump);
            }
        }
        dumps.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(dumps)
    }

    pub fn save_dump(&self, payload: &[u8]) -> io::Result<SavedDump> {
        if payload.len() as u64 > MAX_DUMP_BYTES {
            return Err(io::Error::other(
                "Session state exceeds the 512 MiB export limit",
            ));
        }
        let mut dump = SavedDump {
            id: unique_id(),
            label: self.label.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_millis()
                .try_into()
                .map_err(io::Error::other)?,
            bytes: 0,
        };
        let path = self.dump_path(&dump);
        let mut output = lock_file(&path)?;
        {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(&mut output);
            encoder.write_all(payload)?;
            encoder.finish().map_err(io::Error::other)?;
        }
        output.with_mut(|file| file.sync_all())?;
        dump.bytes = fs::metadata(output.lock_path())?.len();
        if dump.bytes > MAX_DUMP_BYTES {
            return Err(io::Error::other(
                "Compressed session state exceeds the export limit",
            ));
        }
        output.commit().map_err(io::Error::other)?;
        // Publish metadata last so interrupted writes never appear as importable sessions.
        if let Err(error) = write_json(&self.metadata_path(&dump), &dump) {
            let _ = remove_if_present(&path);
            return Err(error);
        }
        Ok(dump)
    }

    /// Reading never consumes the saved state, including when decoding fails.
    pub fn read_dump(&self, dump: &SavedDump) -> io::Result<Vec<u8>> {
        self.check_dump(dump)?;
        decode_dump(self.open_dump(dump)?.take(MAX_DUMP_BYTES + 1))
    }

    pub fn remove_dump(&self, dump: &SavedDump) -> io::Result<()> {
        self.check_dump(dump)?;
        remove_if_present(&self.metadata_path(dump))?;
        remove_if_present(&self.dump_path(dump))
    }

    fn check_dump(&self, dump: &SavedDump) -> io::Result<()> {
        if !valid_dump(dump) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid saved session",
            ));
        }
        if read_json::<SavedDump>(&self.metadata_path(dump))? != *dump {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Saved session identity changed",
            ));
        }
        Ok(())
    }

    fn open_dump(&self, dump: &SavedDump) -> io::Result<fs::File> {
        let file = open_private_file(&self.dump_path(dump), MAX_DUMP_BYTES)?;
        if file.metadata()?.len() != dump.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Saved session size changed",
            ));
        }
        Ok(file)
    }

    fn dump_path(&self, dump: &SavedDump) -> PathBuf {
        self.directory
            .join(format!("{}.{}", dump.id, DUMP_EXTENSION))
    }

    fn metadata_path(&self, dump: &SavedDump) -> PathBuf {
        self.directory
            .join(format!("{}.{}", dump.id, METADATA_EXTENSION))
    }
}

fn valid_dump(dump: &SavedDump) -> bool {
    !dump.id.is_empty()
        && dump.id.len() <= 128
        && dump
            .id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        && dump.label.len() <= 4096
        && dump.bytes <= MAX_DUMP_BYTES
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
    output.with_mut(|file| protect_file(file))?;
    Ok(output)
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut output = lock_file(path)?;
    serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
    output.with_mut(|file| file.sync_all())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rustty-session-dumps-test-{}", unique_id()));
            private_directory(&path).unwrap();
            Self(path)
        }

        fn store(&self, label: &str) -> Store {
            Store::open_in(self.0.clone(), label.into()).unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saved_sessions_survive_exit_and_reads_until_explicit_removal() {
        let directory = TestDirectory::new();
        let source = directory.store("source");
        let state = "scrollback and alternate screen\n".repeat(1000);
        let dump = source.save_dump(state.as_bytes()).unwrap();
        assert_eq!(dump.label, "source");
        assert!(dump.created_at > 1_700_000_000_000);
        assert!(dump.bytes < state.len() as u64);
        drop(source);

        let importer = directory.store("importer");
        assert_eq!(importer.dumps().unwrap(), [dump.clone()]);
        assert_eq!(importer.read_dump(&dump).unwrap(), state.as_bytes());
        assert_eq!(importer.dumps().unwrap(), [dump.clone()]);
        importer.remove_dump(&dump).unwrap();
        assert!(importer.dumps().unwrap().is_empty());
        assert!(!importer.dump_path(&dump).exists());
    }

    #[test]
    fn discovery_sorts_newest_first_and_skips_incomplete_or_invalid_dumps() {
        let directory = TestDirectory::new();
        let store = directory.store("source");
        let mut earlier = store.save_dump(b"earlier").unwrap();
        let later = store.save_dump(b"later").unwrap();
        earlier.created_at = later.created_at - 1;
        write_json(&store.metadata_path(&earlier), &earlier).unwrap();
        let incomplete = store.save_dump(b"incomplete").unwrap();
        fs::remove_file(store.dump_path(&incomplete)).unwrap();
        let oversized = store.save_dump(b"oversized").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(store.dump_path(&oversized))
            .unwrap()
            .set_len(MAX_DUMP_BYTES + 1)
            .unwrap();
        write_json(
            &directory.0.join("invalid.session.json"),
            &serde_json::json!({}),
        )
        .unwrap();
        let staged = lock_file(&directory.0.join("staged.session.json")).unwrap();
        assert_eq!(store.dumps().unwrap(), [later, earlier]);
        drop(staged);
    }

    #[test]
    fn changed_metadata_and_path_traversal_cannot_read_or_remove_other_files() {
        let directory = TestDirectory::new();
        let store = directory.store("source");
        let dump = store.save_dump(b"saved state").unwrap();
        let mut changed = dump.clone();
        changed.label = "another session".into();
        assert!(store.read_dump(&changed).is_err());
        assert!(store.remove_dump(&changed).is_err());
        changed = dump.clone();
        changed.id = "../outside".into();
        assert!(store.read_dump(&changed).is_err());
        assert!(store.remove_dump(&changed).is_err());
        write_json(&directory.0.join("aliased.session.json"), &changed).unwrap();
        assert_eq!(store.dumps().unwrap(), [dump.clone()]);
        assert_eq!(store.read_dump(&dump).unwrap(), b"saved state");
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
        let source = directory.store("source");
        let recipient = directory.store("recipient");
        let canonical = directory.0.join("data/workspace.json");
        let backup = canonical.with_file_name(format!("workspace-{}.json", source.instance_id));

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

    #[cfg(target_os = "macos")]
    #[test]
    fn discovery_and_import_reject_symlinks_hard_links_and_non_private_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = TestDirectory::new();
        let store = directory.store("source");
        let dump = store.save_dump(b"secret").unwrap();
        let path = store.dump_path(&dump);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read_dump(&dump).is_err());
        assert!(store.dumps().unwrap().is_empty());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let linked = directory.0.join("linked");
        fs::hard_link(&path, &linked).unwrap();
        assert!(store.read_dump(&dump).is_err());
        fs::remove_file(&linked).unwrap();
        fs::rename(&path, &linked).unwrap();
        symlink(&linked, &path).unwrap();
        assert!(store.read_dump(&dump).is_err());
        assert!(store.dumps().unwrap().is_empty());

        let public = directory.0.join("public");
        private_directory(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Store::open_in(public, "source".into()).is_err());
        let directory_link = directory.0.join("directory-link");
        symlink(&directory.0, &directory_link).unwrap();
        assert!(Store::open_in(directory_link, "source".into()).is_err());
    }

    #[test]
    fn bounds_decompression_and_rejects_corruption_without_consuming_saved_state() {
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        encoder.write_all(&[0; 1024]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(decode_dump_with_limit(compressed.as_slice(), 100).is_err());

        let directory = TestDirectory::new();
        let store = directory.store("source");
        let mut dump = store.save_dump(b"saved state").unwrap();
        fs::write(store.dump_path(&dump), b"corrupt data").unwrap();
        dump.bytes = 12;
        write_json(&store.metadata_path(&dump), &dump).unwrap();
        assert!(store.read_dump(&dump).is_err());
        assert_eq!(store.dumps().unwrap(), [dump]);
    }
}
