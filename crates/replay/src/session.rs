//! Reusable, bounded acquisition and compiled-runtime caches. Each replay authenticates its own root.
use crate::{Error, Executor, Result, nodes::Nodes};
use parking_lot::Mutex;
use sp_core::H256;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

/// Metadata dependencies are authenticated again at each new state root. Pure metadata
/// exports have an empty dependency list and reuse solely by code hash.
type MetadataRead = (Vec<u8>, Option<Vec<u8>>, Option<H256>);
pub(crate) struct MetadataEntry {
    pub metadata: String,
    pub reads: Vec<MetadataRead>,
    /// Iteration and whole-state/child-root operations bind reuse to this root.
    pub iteration_root: Option<H256>,
    pub proof_nodes: Vec<Vec<u8>>,
}
/// Shared: a hit copies only the metadata text into the block's artifact.
type MetadataCache = BTreeMap<H256, Arc<MetadataEntry>>;

/// Keep one session across blocks. SDK compilation is keyed by authenticated code hash and heap settings.
#[derive(Clone)]
pub struct ReplaySession {
    pub(crate) nodes: Arc<Mutex<Nodes>>,
    pub(crate) hints: Arc<Mutex<VecDeque<BTreeSet<Vec<u8>>>>>,
    // One cold replay teaches the shared proof cache and key predictions before followers
    // start. Otherwise every startup worker repeats the same sequential discovery rounds.
    cold_start: Arc<Mutex<()>>,
    executors: Arc<Mutex<BTreeMap<H256, Arc<Executor>>>>,
    pub(crate) speculative_discovery: bool,
    pub(crate) metadata: Arc<Mutex<MetadataCache>>,
    pub(crate) runtime_override: Option<Arc<Vec<u8>>>,
    /// The last announced override, so repeated loads of one context print once.
    pub(crate) announced_override: Arc<Mutex<Option<crate::RuntimeOverride>>>,
    #[cfg(test)]
    pub(crate) metadata_runs: Arc<std::sync::atomic::AtomicUsize>,
    cache_base: PathBuf,
    /// False when the private Wasm cache directory could not be created.
    wasm_cache: bool,
}
impl ReplaySession {
    /// Proof nodes stay in memory; compiled runtimes persist in the user's private cache.
    pub fn new() -> Result<Self> {
        // Wasmtime loads native code, so another user must not be able to replace this cache.
        let (base, wasm_cache) = match private_cache() {
            Ok(base) => match private_dir(&base.join("wasm")) {
                Ok(()) => (base, true),
                Err(e) => {
                    note!("warning: runtime cache: {e}; runtimes are compiled again each run");
                    (base, false)
                }
            },
            Err(e) => {
                note!("warning: runtime cache: {e}; runtimes are compiled again each run");
                (PathBuf::new(), false)
            }
        };
        let session = Self {
            nodes: Default::default(),
            hints: Default::default(),
            cold_start: Default::default(),
            cache_base: base,
            wasm_cache,
            executors: Default::default(),
            speculative_discovery: true,
            metadata: Default::default(),
            runtime_override: None,
            announced_override: Default::default(),
            #[cfg(test)]
            metadata_runs: Default::default(),
        };
        Ok(session)
    }
    /// Admit one cold replay at a time until successful replay paths are available.
    /// `follow` holds the returned guard through replay; other callers need not use this gate.
    pub fn cold_start(
        &self,
        cancel: &crate::rpc::CancellationToken,
    ) -> Result<Option<parking_lot::MutexGuard<'_, ()>>> {
        self.cold_start_wait(cancel, || {})
    }
    fn cold_start_wait(
        &self,
        cancel: &crate::rpc::CancellationToken,
        waiting: impl FnOnce(),
    ) -> Result<Option<parking_lot::MutexGuard<'_, ()>>> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !self.hints.lock().is_empty() {
            return if cancel.is_cancelled() {
                Err(Error::Cancelled)
            } else {
                Ok(None)
            };
        }
        let mut waiting = Some(waiting);
        loop {
            if let Some(guard) = self
                .cold_start
                .try_lock_for(std::time::Duration::from_millis(25))
            {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                // A replay may have finished while this worker waited.
                return Ok(self.hints.lock().is_empty().then_some(guard));
            }
            if let Some(waiting) = waiting.take() {
                waiting();
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }
    }
    /// Full storage keys and trie paths from the last three successful replays.
    pub(crate) fn predicted_keys(&self) -> BTreeSet<Vec<u8>> {
        self.hints
            .lock()
            .iter()
            .rev()
            .flatten()
            .take(4096)
            .cloned()
            .collect()
    }
    pub(crate) fn remember_paths(&self, paths: BTreeSet<Vec<u8>>) {
        let mut history = self.hints.lock();
        history.push_back(paths);
        while history.len() > 3 {
            history.pop_front();
        }
    }
    /// Execute this Wasm instead of the state's authenticated `:code`. This deliberately
    /// bypasses the code-hash binding to the parent state; every replay reports both hashes.
    /// State proofs and the block's storage-root assertion still apply.
    pub fn with_runtime_override(mut self, wasm: Option<Vec<u8>>) -> Self {
        self.runtime_override = wasm.map(Arc::new);
        self
    }
    /// Disable throw-away speculative passes, to test strict execution-driven paging.
    #[cfg(test)]
    pub(crate) fn with_speculative_discovery(mut self, enabled: bool) -> Self {
        self.speculative_discovery = enabled;
        self
    }
    pub(crate) fn executor_for(&self, wasm: &[u8]) -> Arc<Executor> {
        let hash = H256::from(sp_io::hashing::blake2_256(wasm));
        let mut executors = self.executors.lock();
        if let Some(executor) = executors.get(&hash) {
            return executor.clone();
        }
        // Runtimes in use stay. Once they are idle again, the next new runtime drops as many as
        // it takes to get back to four.
        while executors.len() >= 4
            && let Some(old) = executors
                .iter()
                .find(|(_, e)| Arc::strong_count(e) == 1)
                .map(|(h, _)| *h)
        {
            executors.remove(&old);
        }
        let builder = Executor::builder()
            .with_max_runtime_instances(2)
            .with_runtime_cache_size(4)
            .with_allow_missing_host_functions(false);
        let builder = if self.wasm_cache {
            builder.with_cache_path(self.cache_base.join("wasm").join(format!("{hash:x}")))
        } else {
            builder
        };
        let executor = Arc::new(builder.build());
        executors.insert(hash, executor.clone());
        executor
    }
    /// Keep the newest four compiled runtimes; loaded runtimes stay until workers release them.
    pub(crate) fn prune_runtime_cache(&self) -> Result<()> {
        if !self.wasm_cache {
            return Ok(());
        }
        // No runtime may be loaded from a folder while it is being deleted.
        let executors = self.executors.lock();
        let root = self.cache_base.join("wasm");
        private_dir(&root).map_err(Error::Invalid)?;
        let mut modules = wasm_modules(&root).map_err(|e| Error::Invalid(e.to_string()))?;
        modules.sort_by_key(|m| std::cmp::Reverse(m.modified));
        for module in modules.into_iter().skip(4) {
            if executors.iter().any(|(hash, executor)| {
                Arc::strong_count(executor) > 1 && format!("{hash:x}") == module.name
            }) {
                continue;
            }
            let path = root.join(module.name);
            let removed = if module.directory {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            if let Err(e) = removed
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(Error::Invalid(e.to_string()));
            }
        }
        Ok(())
    }
    /// Proof nodes kept in memory for all replays of this session, within a fixed budget.
    pub fn memory_cache(&self) -> crate::MemoryCacheStats {
        self.nodes.lock().stats()
    }
    pub(crate) fn shared() -> Result<&'static Self> {
        static SESSION: OnceLock<std::result::Result<ReplaySession, String>> = OnceLock::new();
        SESSION
            .get_or_init(|| Self::new().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error::Invalid(e.clone()))
    }
}

/// The user's cache directory, or a per-user folder in the
/// temporary directory. Created private, then resolved through symlinks, so a root that links
/// elsewhere (systemd's `CacheDirectory` with `DynamicUser`) is checked where it really is.
fn private_cache() -> std::result::Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
    {
        Some(dir) => dir.join("polkadot-frame-trace"),
        None => std::env::temp_dir().join(format!(
            "polkadot-frame-trace-{}",
            rustix::process::getuid().as_raw()
        )),
    };
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&base)
        .and_then(|()| base.canonicalize())
        .map_err(|e| format!("{}: {e}", base.display()))
        .and_then(|base| {
            private_dir(&base)
                .and_then(|()| trusted_ancestors(&base))
                .map(|()| base)
        })
}

/// The StrictModes rule of OpenSSH and sudo, for every directory above the cache root: owned
/// by root or this user, and not writable by group or others unless sticky, as `/tmp` is.
/// Otherwise another user could rename the cache away and put their own in its place.
fn trusted_ancestors(root: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::getuid().as_raw();
    for dir in root.ancestors().skip(1) {
        let fail = |why: std::fmt::Arguments| {
            format!(
                "{}: {why}, so another user could replace the cache below it",
                dir.display()
            )
        };
        let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if meta.uid() != 0 && meta.uid() != uid {
            return Err(fail(format_args!(
                "owned by user {}, not by root or this user ({uid})",
                meta.uid()
            )));
        }
        if meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0 {
            return Err(fail(format_args!(
                "group or others can write to it (mode {:o}) and it is not sticky",
                meta.mode() & 0o7777
            )));
        }
    }
    Ok(())
}

/// Creates `dir` with mode 0700 if missing. Refuses a symlink, a directory another user owns,
/// and one that group or others can write.
fn private_dir(dir: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let fail = |why: &dyn std::fmt::Display| format!("{}: {why}", dir.display());
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(fail(&e)),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| fail(&e))?;
    if meta.file_type().is_symlink() {
        return Err(fail(&"is a symlink"));
    }
    if !meta.file_type().is_dir() {
        return Err(fail(&"is not a directory"));
    }
    let uid = rustix::process::getuid().as_raw();
    if meta.uid() != uid {
        return Err(fail(&format_args!(
            "owned by user {}, not by this user ({uid})",
            meta.uid()
        )));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(fail(&format_args!(
            "group or others can write to it (mode {:o})",
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// One entry under `wasm/`: Wasmtime's cache for one runtime, or a stray file.
struct WasmModule {
    name: String,
    directory: bool,
    modified: std::time::SystemTime,
}

/// Wasmtime keeps a few files per runtime, and eviction bounds the runtimes, so this walk is
/// small.
fn wasm_modules(root: &Path) -> std::io::Result<Vec<WasmModule>> {
    fn walk(path: &Path, modified: &mut std::time::SystemTime) -> std::io::Result<()> {
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        *modified = (*modified).max(meta.modified()?);
        if meta.is_dir() {
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            for entry in entries {
                walk(&entry?.path(), modified)?;
            }
        }
        Ok(())
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut modules = Vec::new();
    for entry in entries {
        let entry = entry?;
        let mut modified = std::time::SystemTime::UNIX_EPOCH;
        walk(&entry.path(), &mut modified)?;
        modules.push(WasmModule {
            name: entry.file_name().to_string_lossy().into_owned(),
            directory: entry.file_type()?.is_dir(),
            modified,
        });
    }
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_runtimes_above_four_go_when_the_next_one_loads() {
        let session = ReplaySession::new().expect("session");
        // Six runtimes in use at once: all stay.
        let busy: Vec<_> = (0..6u8).map(|i| session.executor_for(&[i])).collect();
        assert_eq!(session.executors.lock().len(), 6);
        drop(busy);
        // Once idle, one more runtime brings the map back to four, not five.
        let _next = session.executor_for(b"next");
        assert_eq!(session.executors.lock().len(), 4);
    }
    #[test]
    fn cold_followers_wait_for_the_first_replay_unless_cancelled_and_errors_or_panics_release_it() {
        use std::{sync::mpsc, time::Duration};
        let session = ReplaySession::new().expect("session");
        let cancel = crate::rpc::CancellationToken::new();
        let guard = session
            .cold_start(&cancel)
            .expect("admission")
            .expect("cold");
        std::thread::scope(|scope| {
            let (waiting, ready) = mpsc::channel();
            let (done, finished) = mpsc::channel();
            let (session, cancel) = (&session, &cancel);
            let next = scope.spawn(move || {
                assert!(
                    session
                        .cold_start_wait(cancel, || waiting.send(()).expect("waiting"))
                        .expect("admission")
                        .is_none()
                );
                assert!(session.predicted_keys().contains(b"observed".as_slice()));
                done.send(()).expect("done");
            });
            ready
                .recv_timeout(Duration::from_secs(2))
                .expect("contended lock");
            session.remember_paths([b"observed".to_vec()].into_iter().collect());
            assert!(matches!(
                finished.recv_timeout(Duration::from_millis(75)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            drop(guard);
            finished
                .recv_timeout(Duration::from_secs(2))
                .expect("released");
            next.join().expect("follower completed");
        });
        assert!(session.cold_start(&cancel).expect("warm").is_none());

        // A failed replay releases the gate, and so does a panic.
        let session = ReplaySession::new().expect("session");
        let rpc = crate::rpc::Rpc::new("http://127.0.0.1:1").expect("rpc");
        let replay = || -> Result<()> {
            let _guard = session.cold_start(&cancel)?;
            crate::online_with_session(&rpc, "invalid-selector", &session)?;
            Ok(())
        };
        assert!(replay().is_err());
        assert_eq!(rpc.count(), 0);
        assert!(session.cold_start.try_lock().is_some());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = session.cold_start(&cancel).expect("admission");
                panic!("replay panicked");
            }))
            .is_err()
        );
        assert!(session.cold_start.try_lock().is_some());

        // A cancelled waiter leaves while the first replay still holds the gate.
        let cancel = crate::rpc::CancellationToken::new();
        let _guard = session.cold_start(&cancel).expect("admission");
        std::thread::scope(|scope| {
            let (waiting, ready) = mpsc::channel();
            let (done, finished) = mpsc::channel();
            let (session, cancel) = (&session, &cancel);
            scope.spawn(move || {
                assert!(matches!(
                    session.cold_start_wait(cancel, || waiting.send(()).expect("waiting")),
                    Err(Error::Cancelled)
                ));
                done.send(()).expect("done");
            });
            ready
                .recv_timeout(Duration::from_secs(2))
                .expect("contended lock");
            cancel.cancel();
            finished
                .recv_timeout(Duration::from_secs(2))
                .expect("cancelled without gate release");
        });
        assert!(matches!(session.cold_start(&cancel), Err(Error::Cancelled)));
        let rpc =
            crate::rpc::Rpc::with_options("http://127.0.0.1:1", 1, Duration::from_secs(1), cancel)
                .expect("rpc");
        assert!(matches!(
            crate::online_with_session(&rpc, "best", &session),
            Err(Error::Cancelled)
        ));
        assert_eq!(rpc.count(), 0);
    }
    #[test]
    fn wasm_cache_keeps_the_newest_four_and_loaded_runtimes() {
        let dir =
            std::env::temp_dir().join(format!("polkadot-frame-trace-wasm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = ReplaySession::new().expect("session");
        session.cache_base = dir.clone();
        std::fs::create_dir_all(dir.join("wasm")).expect("mkdir");
        let active = session.executor_for(b"runtime");
        let hash = H256::from(sp_io::hashing::blake2_256(b"runtime"));
        let module = dir.join("wasm").join(format!("{hash:x}"));
        std::fs::write(&module, [0; 32]).expect("write");
        for i in 0..6 {
            let path = dir.join("wasm").join(i.to_string());
            std::fs::write(&path, [0; 32]).expect("write");
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open")
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(i + 1))
                .expect("mtime");
        }
        session.prune_runtime_cache().expect("prune");
        assert!(module.exists(), "loaded runtime was deleted");
        assert!(!dir.join("wasm/0").exists());
        assert!(!dir.join("wasm/1").exists());
        assert_eq!(
            std::fs::read_dir(dir.join("wasm")).expect("read").count(),
            5
        );
        drop(active);
        session.prune_runtime_cache().expect("prune");
        assert!(!module.exists());
        assert_eq!(
            std::fs::read_dir(dir.join("wasm")).expect("read").count(),
            4
        );
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
    #[test]
    fn a_cache_others_could_write_or_that_links_elsewhere_is_refused() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
        let dir =
            std::env::temp_dir().join(format!("polkadot-frame-trace-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let used = |root: &Path| {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)
                .expect("mkdir");
            let root = root.canonicalize().expect("canonicalize");
            private_dir(&root)
                .and_then(|()| trusted_ancestors(&root))
                .is_ok()
        };
        // A new root is private; a symlink to it is checked where it leads, as systemd's
        // `CacheDirectory` with `DynamicUser` makes.
        let fresh = dir.join("fresh");
        assert!(used(&fresh));
        let mode = std::fs::metadata(&fresh)
            .expect("root")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        symlink(&fresh, dir.join("alias")).expect("symlink");
        assert!(used(&dir.join("alias")));
        // A `wasm` folder that links elsewhere is not used, and nothing there is deleted.
        let outside = dir.join("outside").join("module");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(outside.join("file"), [0; 64]).expect("write");
        let linked = dir.join("linked");
        std::fs::create_dir(&linked).expect("mkdir");
        symlink(dir.join("outside"), linked.join("wasm")).expect("symlink");
        assert!(private_dir(&linked.join("wasm")).is_err());
        assert!(outside.join("file").exists());
        // A root the group can write is refused whole.
        let shared = dir.join("shared");
        std::fs::create_dir(&shared).expect("mkdir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o775)).expect("chmod");
        assert!(!used(&shared));
        // So is one below a parent the group can write: they could swap the whole cache. A
        // sticky parent, as `/tmp` is, lets no one rename what another user owns.
        let below = shared.join("cache");
        assert!(!used(&below));
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1775)).expect("chmod");
        assert!(used(&below));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
