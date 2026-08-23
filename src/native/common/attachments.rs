//! Owner-scoped, expiring attachment storage.

use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

#[cfg(not(unix))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Url;

pub const DEFAULT_EXPIRATION_SECONDS: u64 = 3_600;
pub const MAX_SAFE_FILENAME_BYTES: usize = 180;

static GLOBAL_ATTACHMENT_STORE: OnceLock<Arc<AttachmentStore>> = OnceLock::new();

#[cfg(not(unix))]
static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Return the process-wide store shared by native tools and the HTTP download
/// route. Stateless deployments must inline content and never initialize it.
pub fn global_attachment_store() -> Result<Arc<AttachmentStore>, String> {
    if env::var("WORKSPACE_MCP_STATELESS_MODE")
        .is_ok_and(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
    {
        return Err(String::from(
            "Attachment storage is unavailable in stateless mode",
        ));
    }
    if let Some(store) = GLOBAL_ATTACHMENT_STORE.get() {
        return Ok(Arc::clone(store));
    }
    let store = Arc::new(AttachmentStore::from_env()?);
    match GLOBAL_ATTACHMENT_STORE.set(Arc::clone(&store)) {
        Ok(()) => Ok(store),
        Err(_) => GLOBAL_ATTACHMENT_STORE
            .get()
            .cloned()
            .ok_or_else(|| String::from("Attachment store initialization failed")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedAttachment {
    pub file_id: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AttachmentMetadata {
    pub file_id: String,
    pub path: PathBuf,
    pub filename: String,
    pub original_filename: Option<String>,
    pub mime_type: String,
    pub size: u64,
    pub owner: String,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
}

struct AttachmentRecord {
    file_id: String,
    path: PathBuf,
    filename: String,
    original_filename: Option<String>,
    mime_type: String,
    size: u64,
    owner: String,
}

/// In-process metadata and private files rooted at `WORKSPACE_ATTACHMENT_DIR`.
#[derive(Debug)]
pub struct AttachmentStore {
    root: PathBuf,
    expiration: Duration,
    entries: Mutex<HashMap<String, AttachmentMetadata>>,
    active: Mutex<HashSet<PathBuf>>,
}

impl AttachmentStore {
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_with_expiration(Duration::from_secs(DEFAULT_EXPIRATION_SECONDS))
    }

    pub fn from_env_with_expiration(expiration: Duration) -> Result<Self, String> {
        let root = match env::var_os("WORKSPACE_ATTACHMENT_DIR") {
            Some(value) => expand_home(PathBuf::from(value))?,
            None => home_directory()?.join(".workspace-mcp").join("attachments"),
        };
        Self::new(root, expiration)
    }

    pub fn new(root: PathBuf, expiration: Duration) -> Result<Self, String> {
        let root = absolute_path(expand_home(root)?)?;
        private_directory(&root)?;
        let root = root.canonicalize().map_err(|error| {
            format!(
                "Failed to resolve attachment storage directory {}: {error}",
                root.display()
            )
        })?;
        Ok(Self {
            root,
            expiration,
            entries: Mutex::new(HashMap::new()),
            active: Mutex::new(HashSet::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Save an in-memory payload, enforcing the caller's byte ceiling.
    pub fn save_bytes(
        &self,
        bytes: &[u8],
        filename: Option<&str>,
        mime_type: Option<&str>,
        owner: &str,
        max_bytes: u64,
    ) -> Result<SavedAttachment, String> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(limit_error(max_bytes));
        }
        let mut stream = self.begin_stream(filename, mime_type, owner, max_bytes)?;
        stream
            .write_all(bytes)
            .map_err(|error| format!("Failed to persist attachment: {error}"))?;
        stream.finish()
    }

    /// Consume an existing regular file without loading it into memory.
    pub fn adopt_path(
        &self,
        source: &Path,
        filename: Option<&str>,
        mime_type: Option<&str>,
        owner: &str,
        max_bytes: u64,
    ) -> Result<SavedAttachment, String> {
        let metadata = fs::metadata(source).map_err(|error| {
            format!(
                "Failed to inspect attachment source {}: {error}",
                source.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(String::from("Attachment source must be a regular file"));
        }
        if metadata.len() > max_bytes {
            return Err(limit_error(max_bytes));
        }

        let mut input = File::open(source).map_err(|error| {
            format!(
                "Failed to open attachment source {}: {error}",
                source.display()
            )
        })?;
        let mut stream = self.begin_stream(filename, mime_type, owner, max_bytes)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = input.read(&mut buffer).map_err(|error| {
                format!(
                    "Failed to read attachment source {}: {error}",
                    source.display()
                )
            })?;
            if read == 0 {
                break;
            }
            stream
                .write_all(&buffer[..read])
                .map_err(|error| format!("Failed to persist attachment: {error}"))?;
        }
        drop(input);
        let saved = stream.finish()?;
        if let Err(error) = fs::remove_file(source) {
            let _ = fs::remove_file(&saved.path);
            self.remove_metadata(&saved.file_id);
            return Err(format!(
                "Failed to consume attachment source {}: {error}",
                source.display()
            ));
        }
        Ok(saved)
    }

    /// Begin a streaming write. The returned writer rejects the chunk that would
    /// exceed `max_bytes` and removes unfinished files when dropped.
    pub fn begin_stream<'a>(
        &'a self,
        filename: Option<&str>,
        mime_type: Option<&str>,
        owner: &str,
        max_bytes: u64,
    ) -> Result<AttachmentStream<'a>, String> {
        let owner = normalize_owner(owner)?;
        let _ = self.sweep_expired();
        let (file_id, stored_filename, path, file) =
            self.create_destination(filename, mime_type)?;
        let registered = match self.active.lock() {
            Ok(mut active) => active.insert(path.clone()),
            Err(_) => {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(String::from("Attachment active-file lock is unavailable"));
            }
        };
        if registered {
            Ok(AttachmentStream {
                store: self,
                file: Some(file),
                file_id,
                path,
                stored_filename,
                original_filename: filename.map(str::to_owned),
                mime_type: mime_type
                    .filter(|value| !value.is_empty())
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                owner,
                max_bytes,
                written: 0,
                finished: false,
            })
        } else {
            drop(file);
            let _ = fs::remove_file(&path);
            Err(String::from("Attachment destination is already active"))
        }
    }

    /// Return metadata only when the ID is live, the file exists, and owner matches.
    /// Missing, expired, and wrong-owner entries are intentionally indistinguishable.
    pub fn lookup_owned(&self, file_id: &str, owner: &str) -> Option<AttachmentMetadata> {
        let owner = normalize_owner(owner).ok()?;
        let now = SystemTime::now();
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get(file_id)?.clone();
        if now > entry.expires_at || !entry.path.is_file() {
            entries.remove(file_id);
            drop(entries);
            let _ = fs::remove_file(entry.path);
            return None;
        }
        if entry.owner != owner {
            return None;
        }
        Some(entry)
    }

    pub fn get_attachment_path(&self, file_id: &str, owner: &str) -> Option<PathBuf> {
        self.lookup_owned_compat(file_id, owner)
            .map(|metadata| metadata.path)
    }

    /// Look up an attachment from the shared metadata map, then from the
    /// owner-isolated on-disk layout used by native services during migration.
    /// The fallback never scans another owner's directory and applies the same
    /// expiry and indistinguishable-not-found behavior.
    pub fn lookup_owned_compat(&self, file_id: &str, owner: &str) -> Option<AttachmentMetadata> {
        if let Some(metadata) = self.lookup_owned(file_id, owner) {
            return Some(metadata);
        }
        if file_id.is_empty()
            || file_id.len() > 128
            || !file_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        let owner = normalize_owner(owner).ok()?;
        let encoded = utf8_percent_encode(&owner, NON_ALPHANUMERIC).to_string();
        let mut hasher = DefaultHasher::new();
        owner.hash(&mut hasher);
        let hashed = format!("owner_{:x}", hasher.finish());
        for directory_name in [encoded, hashed] {
            let directory = self.root.join(directory_name);
            let Ok(canonical) = directory.canonicalize() else {
                continue;
            };
            if !canonical.starts_with(&self.root) || !canonical.is_dir() {
                continue;
            }
            let Ok(entries) = fs::read_dir(&canonical) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                if SystemTime::now()
                    .duration_since(modified)
                    .ok()
                    .is_some_and(|age| age >= self.expiration)
                {
                    let _ = fs::remove_file(path);
                    continue;
                }
                let filename = entry.file_name().to_string_lossy().into_owned();
                let candidate = Path::new(&filename);
                let Some(stem) = candidate.file_stem().map(|value| value.to_string_lossy()) else {
                    continue;
                };
                let suffix = format!("_{file_id}");
                if !stem.ends_with(&suffix) && filename != file_id {
                    continue;
                }
                let Ok(canonical_path) = path.canonicalize() else {
                    continue;
                };
                if !canonical_path.starts_with(&canonical) {
                    return None;
                }
                let mime_type = mime_from_path(candidate).to_owned();
                return Some(AttachmentMetadata {
                    file_id: file_id.to_owned(),
                    path: canonical_path,
                    filename,
                    original_filename: None,
                    mime_type,
                    size: metadata.len(),
                    owner: owner.clone(),
                    created_at: modified,
                    expires_at: modified.checked_add(self.expiration).unwrap_or(modified),
                });
            }
        }
        None
    }

    /// Remove expired metadata and old orphan files based on their modification time.
    pub fn sweep_expired(&self) -> Result<usize, String> {
        let now = SystemTime::now();
        let expired_metadata = {
            let entries = self
                .entries
                .lock()
                .map_err(|_| String::from("Attachment metadata lock is unavailable"))?;
            entries
                .iter()
                .filter(|(_, metadata)| now > metadata.expires_at)
                .map(|(file_id, metadata)| (file_id.clone(), metadata.path.clone()))
                .collect::<Vec<_>>()
        };

        let mut removed_paths = HashSet::new();
        for (_, path) in &expired_metadata {
            if fs::remove_file(path).is_ok() || !path.exists() {
                removed_paths.insert(path.clone());
            }
        }

        let active = self
            .active
            .lock()
            .map_err(|_| String::from("Attachment active-file lock is unavailable"))?
            .clone();
        let entries = fs::read_dir(&self.root).map_err(|error| {
            format!(
                "Failed to scan attachment storage {}: {error}",
                self.root.display()
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            let expired = entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= self.expiration);
            if expired && !active.contains(&path) && fs::remove_file(&path).is_ok() {
                removed_paths.insert(path);
            }
        }

        let mut metadata = self
            .entries
            .lock()
            .map_err(|_| String::from("Attachment metadata lock is unavailable"))?;
        metadata.retain(|_, entry| {
            !removed_paths.contains(&entry.path) && entry.path.is_file() && now <= entry.expires_at
        });
        Ok(removed_paths.len())
    }

    pub fn cleanup_expired(&self) -> Result<usize, String> {
        self.sweep_expired()
    }

    fn create_destination(
        &self,
        filename: Option<&str>,
        mime_type: Option<&str>,
    ) -> Result<(String, String, PathBuf, File), String> {
        for _ in 0..16 {
            let file_id = opaque_id()?;
            let stored_filename = build_save_name(&file_id, filename, mime_type);
            let path = self.root.join(&stored_filename);
            if !path.starts_with(&self.root) {
                return Err(String::from("Attachment path escaped storage root"));
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    secure_file(&path)?;
                    return Ok((file_id, stored_filename, path, file));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Failed to create attachment file {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err(String::from("Failed to allocate attachment filename"))
    }

    fn record(&self, record: AttachmentRecord) -> Result<SavedAttachment, String> {
        let now = SystemTime::now();
        let expires_at = now.checked_add(self.expiration).unwrap_or(now);
        let saved = SavedAttachment {
            file_id: record.file_id.clone(),
            path: record.path.clone(),
        };
        let metadata = AttachmentMetadata {
            file_id: record.file_id.clone(),
            path: record.path,
            filename: record.filename,
            original_filename: record.original_filename,
            mime_type: record.mime_type,
            size: record.size,
            owner: record.owner,
            created_at: now,
            expires_at,
        };
        self.entries
            .lock()
            .map_err(|_| String::from("Attachment metadata lock is unavailable"))?
            .insert(record.file_id, metadata);
        Ok(saved)
    }

    fn remove_metadata(&self, file_id: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(file_id);
        }
    }

    fn remove_active(&self, path: &Path) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(path);
        }
    }
}

/// A file-backed writer that enforces its limit on every write.
pub struct AttachmentStream<'a> {
    store: &'a AttachmentStore,
    file: Option<File>,
    file_id: String,
    path: PathBuf,
    stored_filename: String,
    original_filename: Option<String>,
    mime_type: String,
    owner: String,
    max_bytes: u64,
    written: u64,
    finished: bool,
}

impl AttachmentStream<'_> {
    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn finish(mut self) -> Result<SavedAttachment, String> {
        let mut file = self
            .file
            .take()
            .ok_or_else(|| String::from("Attachment stream is already closed"))?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("Failed to finalize attachment: {error}"))?;
        drop(file);
        secure_file(&self.path)?;
        let saved = self.store.record(AttachmentRecord {
            file_id: self.file_id.clone(),
            path: self.path.clone(),
            filename: self.stored_filename.clone(),
            original_filename: self.original_filename.clone(),
            mime_type: self.mime_type.clone(),
            size: self.written,
            owner: self.owner.clone(),
        })?;
        self.finished = true;
        self.store.remove_active(&self.path);
        Ok(saved)
    }
}

impl Write for AttachmentStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let incoming = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if self.written.saturating_add(incoming) > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                limit_error(self.max_bytes),
            ));
        }
        let written = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("attachment stream is closed"))?
            .write(buffer)?;
        self.written = self
            .written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("attachment stream is closed"))?
            .flush()
    }
}

impl Drop for AttachmentStream<'_> {
    fn drop(&mut self) {
        self.store.remove_active(&self.path);
        if !self.finished {
            self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn normalize_owner(owner: &str) -> Result<String, String> {
    let owner = owner.trim().to_lowercase();
    if owner.is_empty() {
        Err(String::from(
            "An attachment owner is required; attachments cannot be stored without the principal they belong to.",
        ))
    } else {
        Ok(owner)
    }
}

/// Make a filename portable across Unix and Windows and cap its UTF-8 byte length.
pub fn sanitize_attachment_filename(filename: Option<&str>) -> String {
    let Some(filename) = filename.filter(|value| !value.is_empty()) else {
        return String::from("attachment");
    };
    let mut sanitized = String::new();
    for character in filename.chars() {
        let replacement = if is_unicode_space_separator(character) {
            ' '
        } else if character.is_control() || "<>:\"/\\|?*".contains(character) {
            '_'
        } else {
            character
        };
        push_with_limit(&mut sanitized, replacement, MAX_SAFE_FILENAME_BYTES);
    }
    while sanitized.ends_with([' ', '.']) {
        sanitized.pop();
    }
    if sanitized.is_empty() {
        return String::from("attachment");
    }
    let stem = sanitized
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if is_windows_reserved_name(&stem) {
        sanitized.insert(0, '_');
        truncate_utf8(&mut sanitized, MAX_SAFE_FILENAME_BYTES);
    }
    sanitized
}

/// Build an absolute attachment route URL for a stored opaque ID.
pub fn attachment_url(file_id: &str) -> Result<String, String> {
    let base = env::var("WORKSPACE_EXTERNAL_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                env::var("WORKSPACE_MCP_BASE_URI")
                    .unwrap_or_else(|_| String::from("http://localhost")),
                env::var("WORKSPACE_MCP_PORT")
                    .or_else(|_| env::var("PORT"))
                    .unwrap_or_else(|_| String::from("8000"))
            )
        });
    let parsed = Url::parse(base.trim_end_matches('/'))
        .map_err(|_| String::from("Cannot build an absolute attachment URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(String::from("Cannot build an absolute attachment URL"));
    }
    Ok(format!(
        "{}/attachments/{file_id}",
        base.trim_end_matches('/')
    ))
}

fn build_save_name(file_id: &str, filename: Option<&str>, mime_type: Option<&str>) -> String {
    if let Some(filename) = filename.filter(|value| !value.is_empty()) {
        let safe = sanitize_attachment_filename(Some(filename));
        let path = Path::new(&safe);
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("attachment");
        return match path.extension().and_then(|value| value.to_str()) {
            Some(extension) if !extension.is_empty() => format!("{stem}_{file_id}.{extension}"),
            _ => format!("{stem}_{file_id}"),
        };
    }
    format!("{file_id}{}", mime_extension(mime_type.unwrap_or_default()))
}

fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("txt") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("eml") => "message/rfc822",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn mime_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "text/plain" => ".txt",
        "text/html" => ".html",
        _ => "",
    }
}

fn is_unicode_space_separator(character: char) -> bool {
    matches!(
        character as u32,
        0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000
    )
}

fn is_windows_reserved_name(stem: &str) -> bool {
    matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes().first(), Some(b'1'..=b'9'))
            })
}

fn push_with_limit(output: &mut String, character: char, max_bytes: usize) {
    if output.len().saturating_add(character.len_utf8()) <= max_bytes {
        output.push(character);
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn opaque_id() -> Result<String, String> {
    let mut bytes = [0_u8; 28];
    #[cfg(unix)]
    {
        File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| format!("Failed to generate opaque attachment ID: {error}"))?;
    }
    #[cfg(not(unix))]
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let process = u64::from(std::process::id());
        bytes[..16].copy_from_slice(&now.to_be_bytes());
        bytes[16..24].copy_from_slice(&(sequence ^ process.rotate_left(17)).to_be_bytes());
        bytes[24..].copy_from_slice(&std::process::id().to_be_bytes());
    }
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| String::from("Failed to encode attachment ID"))?;
    }
    Ok(encoded)
}

fn private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| {
        format!(
            "Failed to create attachment directory {}: {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "Failed to secure attachment directory {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn secure_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "Failed to secure attachment file {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("Failed to resolve attachment directory: {error}"))
    }
}

fn expand_home(path: PathBuf) -> Result<PathBuf, String> {
    let Some(value) = path.to_str() else {
        return Ok(path);
    };
    if value == "~" {
        return home_directory();
    }
    if let Some(suffix) = value.strip_prefix("~/") {
        return Ok(home_directory()?.join(suffix));
    }
    Ok(path)
}

fn home_directory() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| String::from("Could not determine home directory"))
}

fn limit_error(max_bytes: u64) -> String {
    format!("Attachment exceeds the {max_bytes} byte limit")
}
