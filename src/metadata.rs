//! Acquiring and parsing `cargo metadata --format-version 1` output.
//!
//! The metadata is obtained in one of two ways: by spawning `cargo metadata` (the
//! default) or by reading precomputed JSON from a file or standard input
//! (`--metadata-json`). Either way the bytes land in a [`MetadataBuffer`] that is
//! then parsed by [`parse`] into a [`Meta`] that *borrows* every string and every
//! large sub-array straight from that buffer. The buffer is the arena for the
//! whole run: nothing in [`Meta`] or the graph built on top of it copies a package
//! name, id or manifest path unless the JSON escaped it.
//!
//! The tool never compiles the inspected workspace. `cargo metadata` is the only
//! child process it ever runs, and only on the spawn path.

use std::{
    borrow::Cow,
    fs::File,
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use cargo_metadata::{CargoOpt, MetadataCommand};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{cli::MetadataSource, error::Error};

/// Slack appended after the JSON so a SIMD parser can read past the end without a
/// bounds check. simd-json requires 64 bytes; `serde_json` ignores the padding.
pub const BUFFER_PADDING: usize = 64;

/// Initial capacity reserved for the child's standard output.
///
/// `cargo metadata` emits roughly 4 KiB per package; a mid-sized workspace of a few
/// hundred packages fits without a reallocation, and larger ones double from here.
const SPAWN_INITIAL_CAPACITY: usize = 4 * 1024 * 1024;

/// The default `--cargo-timeout`, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// The smallest budget granted to the post-EOF reap of `cargo metadata`.
///
/// EOF on the pipe means every writer closed it, so cargo has exited or is about to; the
/// remaining share of `--cargo-timeout` is the natural budget, but a run that spent its whole
/// timeout streaming output would otherwise be left with none and a healthy exit would be
/// reported as a timeout. Two seconds is long enough for a process that has already closed
/// its standard output to be reaped on a loaded machine, and short enough that a cargo which
/// closes stdout and then blocks (a stuck credential helper) cannot hang the gate.
const REAP_FLOOR: Duration = Duration::from_secs(2);

/// The longest gap between `try_wait` polls while reaping after EOF.
///
/// The first polls are far shorter, so the common case — a child that is already gone —
/// costs one `waitpid` and no sleep at all.
const REAP_POLL_MAX: Duration = Duration::from_millis(5);

/// How metadata is obtained and, once obtained, rebased.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
#[expect(clippy::struct_excessive_bools, reason = "mirrors cargo's independent boolean flags")]
pub struct MetadataOptions {
    /// The `cargo` executable to spawn. `None` honours `$CARGO` and falls back to `cargo`.
    pub cargo: Option<PathBuf>,
    /// `--manifest-path`; `None` lets cargo search from the current directory.
    pub manifest_path: Option<PathBuf>,
    /// `--features` entries, forwarded verbatim (one `--features` flag per entry).
    pub features: Vec<String>,
    /// `--all-features`.
    pub all_features: bool,
    /// `--no-default-features`.
    pub no_default_features: bool,
    /// `--offline`.
    pub offline: bool,
    /// `--locked`; a gate must never rewrite `Cargo.lock`, so this defaults to `true`.
    pub locked: bool,
    /// Maximum runtime of the child process before it is killed.
    pub timeout: Duration,
    /// Precomputed metadata to read instead of spawning cargo.
    pub source: Option<MetadataSource>,
    /// Directory the JSON's `workspace_root` and member manifests are rebased onto.
    pub workspace_root: Option<PathBuf>,
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self {
            cargo: None,
            manifest_path: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            offline: false,
            locked: true,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            source: None,
            workspace_root: None,
        }
    }
}

impl MetadataOptions {
    /// Overrides the `cargo` executable to spawn.
    #[must_use]
    pub fn with_cargo(mut self, cargo: impl Into<PathBuf>) -> Self {
        self.cargo = Some(cargo.into());
        self
    }

    /// Sets the `--manifest-path` cargo searches from.
    #[must_use]
    pub fn with_manifest_path(mut self, manifest_path: impl Into<PathBuf>) -> Self {
        self.manifest_path = Some(manifest_path.into());
        self
    }

    /// Replaces the `--features` entries, forwarded verbatim.
    #[must_use]
    pub fn with_features(mut self, features: impl IntoIterator<Item: Into<String>>) -> Self {
        self.features = features.into_iter().map(Into::into).collect();
        self
    }

    /// Sets `--all-features`.
    #[must_use]
    pub const fn with_all_features(mut self, all_features: bool) -> Self {
        self.all_features = all_features;
        self
    }

    /// Sets `--no-default-features`.
    #[must_use]
    pub const fn with_no_default_features(mut self, no_default_features: bool) -> Self {
        self.no_default_features = no_default_features;
        self
    }

    /// Sets `--offline`.
    #[must_use]
    pub const fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Sets `--locked`, which defaults to `true` because a gate must never rewrite `Cargo.lock`.
    #[must_use]
    pub const fn with_locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Sets the maximum runtime of the `cargo metadata` child process.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Reads precomputed metadata from `source` instead of spawning cargo.
    #[must_use]
    pub fn with_source(mut self, source: MetadataSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Rebases the document's `workspace_root` and member manifests onto `workspace_root`.
    #[must_use]
    pub fn with_workspace_root(mut self, workspace_root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }
}

/// Raw `cargo metadata` JSON with [`BUFFER_PADDING`] zero bytes of slack after the data.
///
/// The padding is never part of [`MetadataBuffer::as_bytes`]; it exists so that a
/// later swap to a SIMD parser is a one-function change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataBuffer {
    bytes: Vec<u8>,
    len: usize,
    workspace_root: Option<PathBuf>,
}

impl MetadataBuffer {
    /// Wraps already-read JSON, appending the padding.
    #[must_use]
    pub fn from_bytes(mut bytes: Vec<u8>) -> Self {
        let len = bytes.len();
        bytes.resize(len + BUFFER_PADDING, 0);
        Self { bytes, len, workspace_root: None }
    }

    /// Records the directory that [`parse`] rebases the workspace onto.
    #[must_use]
    pub fn with_workspace_root(mut self, workspace_root: Option<PathBuf>) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// The JSON bytes, without the padding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// The directory the workspace is rebased onto, if any.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Total allocation including the padding; useful for reporting.
    #[must_use]
    pub fn padded_len(&self) -> usize {
        self.bytes.len()
    }
}

/// Obtains metadata according to `options`.
///
/// With [`MetadataOptions::source`] set, the JSON is read from the file or standard
/// input; no cargo runs. Otherwise `cargo metadata` is spawned with its standard
/// error inherited (so cargo's own diagnostics reach the user verbatim) and its
/// standard output piped into a reader thread. The main thread waits on a channel
/// with [`MetadataOptions::timeout`]; on timeout the child is killed and reaped,
/// but the reader thread is deliberately *not* joined — `cargo metadata` execs
/// `rustc -vV` and a grandchild may still hold the pipe open past the deadline.
/// After EOF the child is reaped by polling `try_wait`, bounded by whatever is left
/// of [`MetadataOptions::timeout`] and never less than two seconds; a cargo that
/// closes its standard output and then blocks is killed and reported as a timeout
/// rather than hanging the gate.
///
/// # Errors
///
/// - [`Error::MetadataRead`] when the file or standard input cannot be read.
/// - [`Error::CargoMetadataSpawn`] when the child cannot be started.
/// - [`Error::CargoMetadataTimeout`] when the child exceeds the timeout.
/// - [`Error::CargoMetadataRead`] when the pipe fails mid-read or the child
///   cannot be reaped after EOF.
/// - [`Error::CargoMetadataFailed`] when the child exits unsuccessfully.
pub fn acquire(options: &MetadataOptions) -> Result<MetadataBuffer, Error> {
    let buffer = match &options.source {
        Some(MetadataSource::Stdin) => {
            let stdin = io::stdin();
            read_source(stdin.lock(), Path::new("-"), 0)?
        }
        Some(MetadataSource::File(path)) => read_file(path)?,
        None => spawn(options)?,
    };
    Ok(buffer.with_workspace_root(options.workspace_root.clone()))
}

/// Reads precomputed metadata from an arbitrary reader (the `--metadata-json -` path).
///
/// `name` is only used in the error message.
///
/// # Errors
///
/// Returns [`Error::MetadataRead`] when reading fails.
pub fn read_source(
    mut reader: impl Read,
    name: &Path,
    size_hint: usize,
) -> Result<MetadataBuffer, Error> {
    let mut bytes = Vec::with_capacity(size_hint + BUFFER_PADDING);
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| Error::MetadataRead { path: name.to_path_buf(), source })?;
    Ok(MetadataBuffer::from_bytes(bytes))
}

fn read_file(path: &Path) -> Result<MetadataBuffer, Error> {
    let file = File::open(path)
        .map_err(|source| Error::MetadataRead { path: path.to_path_buf(), source })?;
    let size_hint = file.metadata().map_or(0, |metadata| metadata.len());
    let size_hint = usize::try_from(size_hint).unwrap_or(0);
    read_source(file, path, size_hint)
}

/// Builds the `cargo metadata` command line for `options` without running it.
///
/// Exposed so tests and diagnostics can inspect the exact argv; `$CARGO` is honoured
/// through [`MetadataCommand::cargo_command`].
#[must_use]
pub fn cargo_command(options: &MetadataOptions) -> Command {
    let mut command = MetadataCommand::new();
    if let Some(cargo) = &options.cargo {
        command.cargo_path(cargo);
    }
    if let Some(manifest_path) = &options.manifest_path {
        command.manifest_path(manifest_path);
    }
    if options.all_features {
        command.features(CargoOpt::AllFeatures);
    }
    if options.no_default_features {
        command.features(CargoOpt::NoDefaultFeatures);
    }

    let mut other = Vec::with_capacity(2 + 2 * options.features.len());
    if options.locked {
        other.push("--locked".to_owned());
    }
    for feature in &options.features {
        other.push("--features".to_owned());
        other.push(feature.clone());
    }
    if options.offline {
        other.push("--offline".to_owned());
    }
    command.other_options(other);

    let mut command = command.cargo_command();
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    command
}

fn spawn(options: &MetadataOptions) -> Result<MetadataBuffer, Error> {
    let started = Instant::now();
    let mut child =
        cargo_command(options).spawn().map_err(|source| Error::CargoMetadataSpawn { source })?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(Error::CargoMetadataSpawn {
            source: io::Error::new(ErrorKind::BrokenPipe, "the child has no piped stdout"),
        });
    };

    let (sender, receiver) = mpsc::channel();
    // Detached on purpose: see `acquire`. The thread exits on EOF or when the pipe
    // breaks; a failed send after the receiver is gone is harmless.
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(SPAWN_INITIAL_CAPACITY);
        let result = stdout.read_to_end(&mut bytes).map(|_| MetadataBuffer::from_bytes(bytes));
        drop(sender.send(result));
    });

    match receiver.recv_timeout(options.timeout) {
        Ok(Ok(buffer)) => {
            let budget =
                options.timeout.checked_sub(started.elapsed()).unwrap_or_default().max(REAP_FLOOR);
            match reap_bounded(&mut child, budget) {
                Err(source) => Err(Error::CargoMetadataRead { source }),
                Ok(None) => {
                    kill_and_reap(&mut child);
                    Err(Error::CargoMetadataTimeout { timeout: options.timeout })
                }
                Ok(Some(status)) if status.success() => Ok(buffer),
                Ok(Some(status)) => Err(Error::CargoMetadataFailed { status: status.code() }),
            }
        }
        Ok(Err(source)) => {
            kill_and_reap(&mut child);
            Err(Error::CargoMetadataRead { source })
        }
        Err(RecvTimeoutError::Timeout) => {
            kill_and_reap(&mut child);
            Err(Error::CargoMetadataTimeout { timeout: options.timeout })
        }
        Err(RecvTimeoutError::Disconnected) => {
            kill_and_reap(&mut child);
            Err(Error::CargoMetadataRead {
                source: io::Error::other("the metadata reader thread ended without a result"),
            })
        }
    }
}

/// Waits for `child` to exit for at most `budget`, returning `Ok(None)` when it is still
/// running at the end of it.
///
/// The poll interval starts at 100 µs and doubles to [`REAP_POLL_MAX`], so a child that has
/// already exited is reaped without sleeping and one that lingers costs a handful of
/// `waitpid` calls per second.
fn reap_bounded(child: &mut Child, budget: Duration) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + budget;
    let mut interval = Duration::from_micros(100);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(None);
        };
        thread::sleep(interval.min(remaining));
        interval = (interval * 2).min(REAP_POLL_MAX);
    }
}

/// Kills and reaps `child`, tolerating a child that has already exited.
fn kill_and_reap(child: &mut Child) {
    if let Err(error) = child.kill()
        && error.kind() != ErrorKind::InvalidInput
    {
        tracing::debug!(%error, "failed to kill cargo metadata");
    }
    drop(child.wait());
}

/// The subset of `cargo metadata --format-version 1` the policy engine consumes.
///
/// Every string is a [`Cow`] that borrows from the JSON buffer unless the JSON
/// escaped it (Windows `\\` paths); the two large per-package arrays are kept as raw
/// slices and decoded lazily, so parsing allocates one `Vec` per array level and
/// nothing per edge.
#[derive(Debug, Deserialize)]
pub struct Meta<'a> {
    /// Every package in the resolve, workspace members included.
    #[serde(borrow)]
    pub packages: Vec<Pkg<'a>>,
    /// Ids of the workspace members.
    #[serde(borrow)]
    pub workspace_members: Vec<Cow<'a, str>>,
    /// The workspace root directory.
    #[serde(borrow)]
    pub workspace_root: Cow<'a, str>,
    /// The resolved dependency graph; `None` when the JSON came from `--no-deps`.
    #[serde(borrow)]
    pub resolve: Option<Resolve<'a>>,
    /// Non-member `path+` packages left unrebased under `--workspace-root` (§4.21).
    #[serde(skip)]
    pub unrebased_path_deps: u32,
}

/// One `packages[]` entry.
#[derive(Debug, Deserialize)]
pub struct Pkg<'a> {
    /// The package id (`path+file:///…#name@version` or `registry+…#name@version`).
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    /// The package name.
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// The package version.
    #[serde(borrow)]
    pub version: Cow<'a, str>,
    /// Absolute path of the package's `Cargo.toml`.
    #[serde(borrow)]
    pub manifest_path: Cow<'a, str>,
    /// The package source (`registry+…`, `git+…`); `None` for path packages.
    #[serde(borrow, default)]
    pub source: Option<Cow<'a, str>>,
    /// The declared `dependencies` array, undecoded.
    #[serde(borrow)]
    pub dependencies: &'a RawValue,
}

impl Pkg<'_> {
    /// Whether the package comes from a local path (as opposed to a registry or git).
    ///
    /// Cargo reports path packages with a `null` source in every format-version 1
    /// release; the `path+` id prefix is a second, newer signal.
    #[must_use]
    pub fn is_path(&self) -> bool {
        self.source.is_none() || self.id.starts_with("path+")
    }
}

/// The `resolve` object.
#[derive(Debug, Deserialize)]
pub struct Resolve<'a> {
    /// One node per package, in cargo's order (not necessarily `packages[]` order).
    #[serde(borrow)]
    pub nodes: Vec<Node<'a>>,
}

/// One `resolve.nodes[]` entry.
#[derive(Debug, Deserialize)]
pub struct Node<'a> {
    /// The package id this node describes.
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    /// The resolved edges.
    #[serde(borrow)]
    pub deps: Vec<Dep<'a>>,
}

/// One `resolve.nodes[].deps[]` entry.
///
/// `deps[].name` (the possibly renamed crate name) is deliberately not captured:
/// names are always taken from the package `pkg` resolves to (§4.3).
#[derive(Debug, Deserialize)]
pub struct Dep<'a> {
    /// The id of the package this edge points to.
    #[serde(borrow)]
    pub pkg: Cow<'a, str>,
    /// The `dep_kinds` array, undecoded; `None` when absent (pre-1.41 cargo).
    #[serde(borrow, default)]
    pub dep_kinds: Option<&'a RawValue>,
}

/// Parses `buffer` into a borrowing [`Meta`] and applies the `--workspace-root` rebase.
///
/// Parsing uses [`serde_json::from_slice`] over the whole buffer (never a streaming
/// reader) so that every field can borrow. When the buffer carries a workspace
/// root override, [`Meta::rebase`] runs before returning.
///
/// # Errors
///
/// - [`Error::CargoMetadataUnparseable`] for malformed JSON or a missing field.
/// - [`Error::MetadataInvalid`] when `resolve` is `null` or a member cannot be rebased.
/// - [`Error::Usage`] when the workspace root override is not valid UTF-8.
pub fn parse(buffer: &MetadataBuffer) -> Result<Meta<'_>, Error> {
    let mut meta: Meta<'_> = serde_json::from_slice(buffer.as_bytes())
        .map_err(|source| Error::CargoMetadataUnparseable { source })?;
    resolve_of(&meta)?;
    if let Some(root) = buffer.workspace_root() {
        meta.rebase(root)?;
    }
    Ok(meta)
}

/// Returns the resolve, failing closed when it is absent (§4.9).
///
/// # Errors
///
/// Returns [`Error::MetadataInvalid`] when `resolve` is `null`.
pub fn resolve_of<'m, 'a>(meta: &'m Meta<'a>) -> Result<&'m Resolve<'a>, Error> {
    meta.resolve.as_ref().ok_or_else(|| Error::MetadataInvalid {
        message: "`resolve` is null; the metadata was generated with --no-deps".to_owned(),
    })
}

impl Meta<'_> {
    /// Rebases `workspace_root` and every `path+` manifest onto `dir` (§1.2).
    ///
    /// The prefix comparison is `/`-separated: a manifest rebases when it equals the
    /// JSON's `workspace_root` or starts with it followed by `/`. Members must rebase
    /// — cargo guarantees they live under the root, so a failure is a fail-closed
    /// assertion. Non-member `path+` packages outside the root are left untouched and
    /// counted in [`Meta::unrebased_path_deps`]; their manifests are never opened.
    ///
    /// Only `manifest_path` and `workspace_root` are rewritten. The `path+file://…`
    /// prefix inside a package `id` (and inside `workspace_members` and resolve
    /// edges) keeps the pre-rebase path: ids are only ever compared with each other,
    /// so rewriting them buys nothing and a diagnostic that prints an id shows the
    /// path the JSON was generated with.
    ///
    /// # Errors
    ///
    /// - [`Error::Usage`] when `dir` is not valid UTF-8 (it has to be spliced into
    ///   the JSON's string fields).
    /// - [`Error::MetadataInvalid`] when a workspace member's manifest is not under
    ///   the JSON's `workspace_root`.
    pub fn rebase(&mut self, dir: &Path) -> Result<(), Error> {
        let Some(dir) = dir.to_str() else {
            return Err(Error::Usage {
                message: format!("--workspace-root {} is not valid UTF-8", dir.display()),
            });
        };
        let dir = if dir.len() > 1 { dir.trim_end_matches('/') } else { dir };
        let old_root = std::mem::take(&mut self.workspace_root);
        let mut unrebased = 0_u32;

        for package in &mut self.packages {
            if !package.is_path() {
                continue;
            }
            if let Some(rebased) = rebase_path(&package.manifest_path, &old_root, dir) {
                package.manifest_path = Cow::Owned(rebased);
            } else if self.workspace_members.contains(&package.id) {
                return Err(Error::MetadataInvalid {
                    message: format!(
                        "workspace member `{}` manifest `{}` is not under the workspace root `{old_root}`",
                        package.name, package.manifest_path
                    ),
                });
            } else {
                unrebased += 1;
            }
        }

        self.workspace_root = Cow::Owned(dir.to_owned());
        self.unrebased_path_deps = unrebased;
        Ok(())
    }
}

/// Rewrites `path` from under `old_root` to under `new_root`, or returns `None`
/// when `path` is not `old_root` itself or a `/`-separated descendant of it.
fn rebase_path(path: &str, old_root: &str, new_root: &str) -> Option<String> {
    let rest = path.strip_prefix(old_root)?;
    if rest.is_empty() {
        return Some(new_root.to_owned());
    }
    let rest = rest.strip_prefix('/')?;
    let new_root = new_root.trim_end_matches('/');
    Some(format!("{new_root}/{rest}"))
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
pub(crate) mod tests;
