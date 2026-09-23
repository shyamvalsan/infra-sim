//! Versioned raw producer-boundary recordings. No downstream verdicts are stored.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const DEFAULT_CAP_BYTES: u64 = 1024 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"ISIMREC1";
const MAX_METADATA: usize = 64 * 1024;
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Producer {
    Metrics,
    Journal,
    Otlp,
    Exporters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Start,
    Stop,
    Control,
    Metrics,
    Journal,
    OtlpLogs,
    OtlpTraces,
    Exporter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub producer: Producer,
    pub session: String,
    pub observed_ns: u64,
    pub kind: Kind,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub origin_ns: u64,
    pub cap_bytes: u64,
    #[serde(default)]
    pub expected_producers: Vec<Producer>,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub bytes: u64,
    pub cap_bytes: u64,
    pub finalized: bool,
    pub incomplete: Option<String>,
}

pub struct Frame {
    pub metadata: Metadata,
    pub payload: Vec<u8>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

#[cfg(unix)]
fn shared_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o660))
}
#[cfg(not(unix))]
fn shared_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

fn lock(dir: &Path) -> io::Result<File> {
    let path = dir.join("append.lock");
    let file = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => {
            shared_permissions(&file)?;
            file
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            OpenOptions::new().read(true).write(true).open(&path)?
        }
        Err(error) => return Err(error),
    };
    // Only writer threads, producer startup, status and finalization wait
    // here; telemetry never does, so the bound can tolerate slow storage while
    // still refusing to hang behind a stopped holder.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    }
}

fn atomic_write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}-{}.tmp", std::process::id()));
    let mut file = File::create(&tmp)?;
    shared_permissions(&file)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, dir.join(name))?;
    File::open(dir)?.sync_all()
}

fn committed(dir: &Path) -> io::Result<u64> {
    let mut bytes = [0; 8];
    let mut file = File::open(dir.join("committed"))?;
    file.read_exact(&mut bytes)?;
    if file.metadata()?.len() != 8 {
        return Err(invalid("invalid committed recording length"));
    }
    let length = u64::from_le_bytes(bytes);
    if length < MAGIC.len() as u64 {
        return Err(invalid("invalid committed recording length"));
    }
    Ok(length)
}

pub fn manifest(dir: &Path) -> io::Result<Manifest> {
    let mut bytes = Vec::new();
    File::open(dir.join("manifest.json"))?
        .take(MAX_METADATA as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METADATA {
        return Err(invalid("recording manifest is too large"));
    }
    let value: Manifest = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    if value.version != 1 || value.cap_bytes < MAGIC.len() as u64 {
        return Err(invalid("unsupported recording version or capacity"));
    }
    Ok(value)
}

pub fn status(dir: &Path) -> io::Result<Status> {
    // Finalization is the last write under the lock and every later append is
    // refused, so a finalized archive is immutable and replays from read-only
    // media without the writer lock.
    if dir.join("finalized").is_file() {
        return status_locked(dir);
    }
    let _guard = lock(dir).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot lock recording {}: {error}", dir.display()),
        )
    })?;
    status_locked(dir)
}

fn status_locked(dir: &Path) -> io::Result<Status> {
    let config = manifest(dir)?;
    let mut incomplete = match File::open(dir.join("incomplete")) {
        Ok(file) => {
            let mut reason = String::new();
            file.take(512).read_to_string(&mut reason)?;
            // The reason write can fail after the marker exists (a full disk):
            // an empty reason must never read as complete to JSON consumers.
            if reason.trim().is_empty() {
                reason = "recording marked incomplete; its reason could not be persisted".into();
            }
            Some(reason)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let bytes = std::fs::metadata(dir.join("events.bin"))?.len();
    if bytes != committed(dir)? || bytes > config.cap_bytes {
        incomplete = Some(
            "recording has an interrupted or invalid suffix; committed prefix preserved".into(),
        );
    }
    Ok(Status {
        bytes,
        cap_bytes: config.cap_bytes,
        finalized: dir.join("finalized").is_file(),
        incomplete,
    })
}

/// Queued capture memory. Beyond it recording stops rather than stall telemetry.
const MAX_PENDING: u64 = 64 * 1024 * 1024;
/// A backlog is written in bounded chunks, each with one durable commit.
const MAX_BATCH: u64 = 8 * 1024 * 1024;

struct Pending {
    metadata: Vec<u8>,
    payload: Vec<u8>,
}

impl Pending {
    fn size(&self) -> u64 {
        12 + self.metadata.len() as u64 + self.payload.len() as u64
    }
}

enum Message {
    Frame(Pending),
    Flush(std::sync::mpsc::SyncSender<()>),
}

fn encode(
    producer: Producer,
    session: &str,
    kind: Kind,
    target: &str,
    payload: &[u8],
) -> io::Result<Pending> {
    if payload.len() > MAX_PAYLOAD {
        return Err(invalid("recording frame exceeds maximum payload size"));
    }
    // Observed at capture, not at write, so batching never shifts replay timing.
    let metadata = serde_json::to_vec(&Metadata {
        producer,
        session: session.into(),
        observed_ns: now_ns(),
        kind,
        target: target.into(),
    })?;
    if metadata.len() > MAX_METADATA {
        return Err(invalid("recording frame metadata is too large"));
    }
    Ok(Pending {
        metadata,
        payload: payload.to_vec(),
    })
}

/// State shared by a producer's capture calls and its writer thread.
struct Shared {
    dir: PathBuf,
    cap: u64,
    failed: AtomicBool,
    pending: AtomicU64,
}

impl Shared {
    fn mark_incomplete(&self, reason: &str) {
        self.failed.store(true, Ordering::Release);
        eprintln!("infra-sim recording: incomplete: {reason}");
        if let Ok(mut file) = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.dir.join("incomplete"))
        {
            let text: String = reason.chars().take(400).collect();
            if let Err(error) = file
                .write_all(text.as_bytes())
                .and_then(|_| file.sync_all())
            {
                eprintln!("infra-sim recording: cannot persist failure marker: {error}");
            }
        }
    }

    /// Appends whole frames under one lock and one durable commit. Frames that
    /// fit before the cap are committed; the rest are refused, so the recording
    /// is always an exact prefix of what was captured.
    fn write_batch(&self, batch: &[Pending]) -> io::Result<()> {
        let _guard = lock(&self.dir)?;
        if self.dir.join("incomplete").exists() {
            self.failed.store(true, Ordering::Release);
            return Ok(());
        }
        if self.dir.join("finalized").exists() {
            return Err(invalid("output arrived after recording finalization"));
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.dir.join("events.bin"))?;
        let length = file.metadata()?.len();
        if length != committed(&self.dir)? {
            return Err(invalid(
                "recording append interrupted; committed prefix preserved",
            ));
        }
        let mut end = length;
        let mut result = Ok(());
        for frame in batch {
            if end.saturating_add(frame.size()) > self.cap {
                result = Err(invalid(
                    "recording byte cap reached; recorded prefix preserved",
                ));
                break;
            }
            file.write_all(&(frame.metadata.len() as u32).to_le_bytes())?;
            file.write_all(&(frame.payload.len() as u64).to_le_bytes())?;
            file.write_all(&frame.metadata)?;
            file.write_all(&frame.payload)?;
            end += frame.size();
        }
        if end != length {
            file.sync_data()?;
            atomic_write(&self.dir, "committed", &end.to_le_bytes())?;
        }
        result
    }

    /// Group commit: whatever queued while the previous batch was being made
    /// durable is written together, so storage latency bounds throughput per
    /// batch rather than per frame.
    fn run_writer(&self, receiver: std::sync::mpsc::Receiver<Message>) {
        while let Ok(first) = receiver.recv() {
            let mut batch = Vec::new();
            let mut acknowledgements = Vec::new();
            let mut bytes = 0;
            let mut next = Some(first);
            while let Some(message) = next.take() {
                match message {
                    Message::Frame(frame) => {
                        bytes += frame.size();
                        batch.push(frame);
                    }
                    Message::Flush(done) => acknowledgements.push(done),
                }
                if bytes < MAX_BATCH {
                    next = receiver.try_recv().ok();
                }
            }
            if !batch.is_empty() && !self.failed.load(Ordering::Acquire) {
                if let Err(error) = self.write_batch(&batch) {
                    self.mark_incomplete(&error.to_string());
                }
            }
            self.pending.fetch_sub(bytes, Ordering::AcqRel);
            for done in acknowledgements {
                let _ = done.send(());
            }
        }
    }
}

/// Every process uses the same lock and file-size check, including after restart.
/// Capture only queues; a per-producer writer thread owns all storage waits.
pub struct Recorder {
    shared: Arc<Shared>,
    producer: Producer,
    session: String,
    sender: Option<std::sync::mpsc::Sender<Message>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Recorder {
    pub fn open(dir: &Path, producer: Producer, cap: u64) -> io::Result<Arc<Self>> {
        Self::open_expected(dir, producer, cap, &[])
    }

    fn open_expected(
        dir: &Path,
        producer: Producer,
        cap: u64,
        expected: &[Producer],
    ) -> io::Result<Arc<Self>> {
        if cap < MAGIC.len() as u64 {
            return Err(invalid("recording cap is too small"));
        }
        std::fs::create_dir_all(dir)?;
        let guard = lock(dir)?;
        if dir.join("finalized").exists() {
            return Err(invalid("recording is finalized"));
        }
        let config_path = dir.join("manifest.json");
        let existing = config_path.exists();
        if existing {
            let config = manifest(dir)?;
            if config.cap_bytes != cap || config.expected_producers != expected {
                return Err(invalid("recording configuration differs between producers"));
            }
        } else {
            if dir.join("events.bin").exists() || dir.join("committed").exists() {
                return Err(invalid("recording data exists without its manifest"));
            }
            let config = Manifest {
                version: 1,
                origin_ns: now_ns(),
                cap_bytes: cap,
                expected_producers: expected.to_vec(),
            };
            atomic_write(dir, "manifest.json", &serde_json::to_vec(&config)?)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(dir.join("events.bin"))?;
        if file.metadata()?.len() == 0 {
            if existing {
                return Err(invalid(
                    "existing recording is empty or missing; refusing to reset its prefix",
                ));
            }
            shared_permissions(&file)?;
            file.write_all(MAGIC)?;
            file.sync_all()?;
            atomic_write(dir, "committed", &(MAGIC.len() as u64).to_le_bytes())?;
        } else {
            let mut magic = [0u8; 8];
            file.read_exact(&mut magic)?;
            if &magic != MAGIC {
                return Err(invalid("invalid recording header"));
            }
        }
        let initial_status = status_locked(dir)?;
        let shared = Arc::new(Shared {
            dir: dir.into(),
            cap,
            failed: AtomicBool::new(false),
            pending: AtomicU64::new(0),
        });
        if let Some(reason) = initial_status.incomplete {
            shared.mark_incomplete(&reason);
        }
        drop(guard);
        let session = format!("{}-{}", std::process::id(), now_ns());
        // The start frame is written synchronously so a producer that cannot
        // record learns it before running, as before batching.
        if !shared.failed.load(Ordering::Acquire) {
            let start = encode(producer, &session, Kind::Start, "", b"")?;
            if let Err(error) = shared.write_batch(std::slice::from_ref(&start)) {
                shared.mark_incomplete(&error.to_string());
            }
        }
        if shared.failed.load(Ordering::Acquire) && !dir.join("incomplete").exists() {
            return Err(invalid("cannot persist recording start or failure state"));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let writer = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("infra-sim-recorder".into())
                .spawn(move || shared.run_writer(receiver))?
        };
        Ok(Arc::new(Self {
            shared,
            producer,
            session,
            sender: Some(sender),
            writer: Some(writer),
        }))
    }

    /// Recording is opt-in outside managed simulations. Configuration is not argv.
    pub fn from_environment(producer: Producer) -> io::Result<Option<Arc<Self>>> {
        let Some(dir) = std::env::var_os("INFRA_SIM_RECORD_DIR") else {
            return Ok(None);
        };
        let cap = match std::env::var("INFRA_SIM_RECORD_MAX_BYTES") {
            Ok(value) => value
                .parse()
                .map_err(|_| invalid("INFRA_SIM_RECORD_MAX_BYTES must be an integer"))?,
            Err(std::env::VarError::NotPresent) => DEFAULT_CAP_BYTES,
            Err(error) => return Err(invalid(error.to_string())),
        };
        let expected = match std::env::var("INFRA_SIM_RECORD_EXPECTED") {
            Ok(value) => value
                .split(',')
                .map(|name| match name {
                    "metrics" => Ok(Producer::Metrics),
                    "journal" => Ok(Producer::Journal),
                    "otlp" => Ok(Producer::Otlp),
                    "exporters" => Ok(Producer::Exporters),
                    _ => Err(invalid("invalid INFRA_SIM_RECORD_EXPECTED producer")),
                })
                .collect::<io::Result<Vec<_>>>()?,
            Err(std::env::VarError::NotPresent) => Vec::new(),
            Err(error) => return Err(invalid(error.to_string())),
        };
        Self::open_expected(Path::new(&dir), producer, cap, &expected).map(Some)
    }

    pub fn mark_incomplete(&self, reason: &str) {
        self.shared.mark_incomplete(reason);
    }

    /// Failure to record must never manufacture a successful complete recording,
    /// and must never block the telemetry path that called it.
    pub fn capture(&self, kind: Kind, target: &str, payload: &[u8]) {
        if self.shared.failed.load(Ordering::Acquire) {
            return;
        }
        let frame = match encode(self.producer, &self.session, kind, target, payload) {
            Ok(frame) => frame,
            Err(error) => return self.mark_incomplete(&error.to_string()),
        };
        let size = frame.size();
        if self.shared.pending.fetch_add(size, Ordering::AcqRel) + size > MAX_PENDING {
            self.shared.pending.fetch_sub(size, Ordering::AcqRel);
            return self.mark_incomplete("recording writer fell behind; recorded prefix preserved");
        }
        let sent = self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(Message::Frame(frame)).is_ok());
        if !sent {
            self.shared.pending.fetch_sub(size, Ordering::AcqRel);
            self.mark_incomplete("recording writer stopped");
        }
    }

    /// Blocks until every frame captured so far has been written or refused.
    pub fn flush(&self) {
        let (done, wait) = std::sync::mpsc::sync_channel(1);
        if let Some(sender) = &self.sender {
            if sender.send(Message::Flush(done)).is_ok() {
                let _ = wait.recv();
            }
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.capture(Kind::Stop, "", b"");
        // Closing the queue ends the writer after it drains, so a clean stop
        // persists every frame captured before it.
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Streaming reader bounds all allocations and refuses truncated frames.
pub struct Reader {
    file: std::io::Take<File>,
    end: u64,
}
impl Reader {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let config = manifest(dir)?;
        let length = committed(dir)?;
        if length > config.cap_bytes {
            return Err(invalid("committed recording exceeds capacity"));
        }
        let mut file = File::open(dir.join("events.bin"))?;
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("invalid recording header"));
        }
        if file.metadata()?.len() < length {
            return Err(invalid("committed recording is truncated"));
        }
        Ok(Self {
            file: file.take(length - MAGIC.len() as u64),
            end: length,
        })
    }
    pub fn position(&mut self) -> io::Result<u64> {
        self.file.get_mut().stream_position()
    }

    pub fn seek_frame(&mut self, offset: u64) -> io::Result<()> {
        if offset < MAGIC.len() as u64 || offset > self.end {
            return Err(invalid("recording offset outside committed stream"));
        }
        self.file.get_mut().seek(SeekFrom::Start(offset))?;
        self.file.set_limit(self.end - offset);
        Ok(())
    }

    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        let mut lengths = [0u8; 12];
        if self.file.read(&mut lengths[..1])? == 0 {
            return Ok(None);
        }
        self.file.read_exact(&mut lengths[1..])?;
        let meta_len = u32::from_le_bytes(lengths[..4].try_into().unwrap()) as usize;
        let payload_len = u64::from_le_bytes(lengths[4..].try_into().unwrap());
        if meta_len > MAX_METADATA || payload_len > MAX_PAYLOAD as u64 {
            return Err(invalid("invalid recording frame lengths"));
        }
        if (meta_len as u64).saturating_add(payload_len) > self.file.limit() {
            return Err(invalid("truncated recording frame"));
        }
        let mut metadata = vec![0; meta_len];
        self.file.read_exact(&mut metadata)?;
        let metadata = serde_json::from_slice(&metadata).map_err(|e| invalid(e.to_string()))?;
        let mut payload = vec![0; payload_len as usize];
        self.file.read_exact(&mut payload)?;
        Ok(Some(Frame { metadata, payload }))
    }
}

/// Re-derives completeness from the committed stream itself, so a copied or
/// hand-edited finalized marker can never vouch for sessions it did not see.
/// Returns the reason the recording is incomplete, or `None` when every
/// expected producer started and every session stopped cleanly.
pub fn verify(dir: &Path) -> io::Result<Option<&'static str>> {
    let mut reader = Reader::open(dir)?;
    let mut sessions = std::collections::BTreeSet::new();
    let mut starts = 0;
    let expected = manifest(dir)?.expected_producers;
    let mut observed = Vec::new();
    while let Some(frame) = reader.next_frame()? {
        match frame.metadata.kind {
            Kind::Start => {
                starts += 1;
                observed.push(frame.metadata.producer);
                sessions.insert(frame.metadata.session);
            }
            Kind::Stop => {
                sessions.remove(&frame.metadata.session);
            }
            _ => {}
        }
    }
    if expected.iter().any(|producer| !observed.contains(producer)) {
        return Ok(Some("an expected producer never started recording"));
    }
    if starts == 0 || !sessions.is_empty() {
        return Ok(Some(
            "recording has no producer session or a session without clean shutdown",
        ));
    }
    Ok(None)
}

/// When the recording sits in a teardown archive, every file in it must match
/// the archive's SHA-256 inventory and none may be added or missing. This
/// detects corruption and partial copies; the unsigned inventory is not proof
/// against deliberate forgery. A standalone recording has no inventory.
pub fn verify_archive_inventory(dir: &Path) -> Result<(), String> {
    let Some(archive) = dir.parent() else {
        return Ok(());
    };
    let inventory = archive.join("archive.json");
    let raw = match std::fs::read(&inventory) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", inventory.display())),
    };
    let manifest: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("invalid archive inventory: {error}"))?;
    let hashes = manifest["sha256"]
        .as_object()
        .ok_or("archive inventory has no sha256 map")?;
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("recording directory name is not valid UTF-8")?;
    let prefix = format!("{name}/");
    let listed: std::collections::BTreeMap<&str, &serde_json::Value> = hashes
        .iter()
        .filter_map(|(path, hash)| path.strip_prefix(&prefix).map(|file| (file, hash)))
        .collect();
    if listed.is_empty() {
        return Err("archive inventory does not cover this recording".into());
    }
    let mut present = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("archived recording contains a non-file entry".into());
        }
        present.insert(entry.file_name().to_string_lossy().into_owned());
    }
    for file in &present {
        if !listed.contains_key(file.as_str()) {
            return Err(format!(
                "archived recording file {file} is not in the inventory"
            ));
        }
    }
    for (file, expected) in listed {
        if !present.contains(file) {
            return Err(format!("archived recording file {file} is missing"));
        }
        if expected.as_str() != Some(crate::lint_evidence::file_hash(&dir.join(file))?.as_str()) {
            return Err(format!(
                "archived recording file {file} does not match its checksum"
            ));
        }
    }
    Ok(())
}

/// Call only once the owning simulation's producer processes have stopped.
pub fn finalize(dir: &Path) -> io::Result<Status> {
    let _guard = lock(dir)?;
    if let Some(reason) = verify(dir)? {
        if !dir.join("incomplete").exists() {
            atomic_write(dir, "incomplete", reason.as_bytes())?;
        }
    }
    File::open(dir.join("events.bin"))?.sync_all()?;
    atomic_write(dir, "finalized", b"1\n")?;
    status_locked(dir)
}

/// Place inside a buffer so frames represent flushed chunks, not formatted fields.
pub struct RecordedWriter<W> {
    inner: W,
    recorder: Option<Arc<Recorder>>,
    kind: Kind,
    target: String,
}
impl<W> RecordedWriter<W> {
    pub fn new(inner: W, recorder: Option<Arc<Recorder>>, kind: Kind, target: String) -> Self {
        Self {
            inner,
            recorder,
            kind,
            target,
        }
    }
}
impl<W: Write> Write for RecordedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.inner.write_all(bytes) {
            if let Some(recorder) = &self.recorder {
                recorder.mark_incomplete("producer output write failed");
            }
            return Err(error);
        }
        if let Some(recorder) = &self.recorder {
            recorder.capture(self.kind, &self.target, bytes);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        if result.is_err() {
            if let Some(recorder) = &self.recorder {
                recorder.mark_incomplete("producer output flush failed");
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Removed on drop so repeated test runs cannot fill a small tmpfs.
    struct TempDir(PathBuf);
    impl std::ops::Deref for TempDir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }
    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn directory() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "infra-sim-recording-{}-{}",
            std::process::id(),
            now_ns()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    #[test]
    fn managed_recordings_require_every_expected_producer() {
        let expected = [Producer::Metrics, Producer::Journal];
        let missing = directory();
        drop(Recorder::open_expected(&missing, Producer::Metrics, 100_000, &expected).unwrap());
        assert!(finalize(&missing)
            .unwrap()
            .incomplete
            .unwrap()
            .contains("expected producer"));

        let complete = directory();
        drop(Recorder::open_expected(&complete, Producer::Metrics, 100_000, &expected).unwrap());
        assert!(Recorder::open(&complete, Producer::Journal, 100_000).is_err());
        drop(Recorder::open_expected(&complete, Producer::Journal, 100_000, &expected).unwrap());
        assert!(finalize(&complete).unwrap().incomplete.is_none());
    }

    #[test]
    fn concurrent_producers_preserve_exact_payloads_and_one_cap() {
        let dir = directory();
        let cap = 100_000;
        let a = Recorder::open(&dir, Producer::Metrics, cap).unwrap();
        let b = Recorder::open(&dir, Producer::Journal, cap).unwrap();
        std::thread::scope(|scope| {
            for recorder in [&a, &b] {
                scope.spawn(move || {
                    for _ in 0..50 {
                        recorder.capture(Kind::Journal, "sim-test", b"raw\0\n\xff");
                    }
                });
            }
        });
        drop(a);
        drop(b);
        let status = finalize(&dir).unwrap();
        assert!(status.finalized && status.incomplete.is_none() && status.bytes <= cap);
        let mut reader = Reader::open(&dir).unwrap();
        let mut count = 0;
        while let Some(frame) = reader.next_frame().unwrap() {
            if frame.metadata.kind == Kind::Journal {
                assert_eq!(frame.payload, b"raw\0\n\xff");
                count += 1;
            }
        }
        assert_eq!(count, 100);
    }
    #[test]
    fn starting_producers_during_appends_does_not_report_false_corruption() {
        let dir = directory();
        let cap = 1_000_000;
        let writer = Recorder::open(&dir, Producer::Metrics, cap).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..100 {
                    writer.capture(Kind::Metrics, "", b"sample");
                }
            });
            for _ in 0..20 {
                let other = Recorder::open(&dir, Producer::Journal, cap).unwrap();
                assert!(status(&dir).unwrap().incomplete.is_none());
                drop(other);
            }
        });
        drop(writer);
        assert!(finalize(&dir).unwrap().incomplete.is_none());
    }

    #[test]
    fn cap_preserves_prefix_and_remains_incomplete_after_restart() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 1000).unwrap();
        recorder.capture(Kind::Metrics, "", b"first");
        recorder.flush();
        let before = std::fs::read(dir.join("events.bin")).unwrap();
        recorder.capture(Kind::Metrics, "", &[0; 1000]);
        recorder.capture(Kind::Metrics, "", b"later");
        drop(recorder);
        drop(Recorder::open(&dir, Producer::Journal, 1000).unwrap());
        assert_eq!(std::fs::read(dir.join("events.bin")).unwrap(), before);
        assert!(finalize(&dir).unwrap().incomplete.unwrap().contains("cap"));
    }
    #[cfg(unix)]
    #[test]
    fn finalized_recording_status_needs_no_writable_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = directory();
        drop(Recorder::open(&dir, Producer::Metrics, 10000).unwrap());
        finalize(&dir).unwrap();
        std::fs::remove_file(dir.join("append.lock")).unwrap();
        let readonly = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&dir, readonly).unwrap();
        let result = status(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = result.unwrap();
        assert!(status.finalized && status.incomplete.is_none());
        assert!(!dir.join("append.lock").exists());
    }

    #[test]
    fn replay_inventory_rejects_changed_added_or_missing_files() {
        let archive = directory();
        let dir = archive.join("recording");
        drop(Recorder::open(&dir, Producer::Metrics, 10000).unwrap());
        finalize(&dir).unwrap();
        assert!(
            verify_archive_inventory(&dir).is_ok(),
            "no inventory: standalone"
        );
        let mut hashes = serde_json::Map::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            hashes.insert(
                format!("recording/{name}"),
                crate::lint_evidence::file_hash(&path).unwrap().into(),
            );
        }
        let manifest = serde_json::json!({"version": 1, "sha256": hashes});
        std::fs::write(archive.join("archive.json"), manifest.to_string()).unwrap();
        assert!(verify_archive_inventory(&dir).is_ok());

        let events = std::fs::read(dir.join("events.bin")).unwrap();
        let mut flipped = events.clone();
        *flipped.last_mut().unwrap() ^= 1;
        std::fs::write(dir.join("events.bin"), &flipped).unwrap();
        assert!(verify_archive_inventory(&dir)
            .unwrap_err()
            .contains("checksum"));
        std::fs::write(dir.join("events.bin"), &events).unwrap();

        std::fs::write(dir.join("extra"), b"").unwrap();
        assert!(verify_archive_inventory(&dir)
            .unwrap_err()
            .contains("not in the inventory"));
        std::fs::remove_file(dir.join("extra")).unwrap();

        std::fs::remove_file(dir.join("finalized")).unwrap();
        assert!(verify_archive_inventory(&dir)
            .unwrap_err()
            .contains("missing"));
    }

    #[test]
    fn capture_never_waits_for_storage() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 1_000_000).unwrap();
        let held = lock(&dir).unwrap();
        let started = Instant::now();
        for _ in 0..200 {
            recorder.capture(Kind::Metrics, "", b"sample");
        }
        assert!(started.elapsed() < Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
        drop(recorder);
        assert!(finalize(&dir).unwrap().incomplete.is_none());
        let mut reader = Reader::open(&dir).unwrap();
        let mut samples = 0;
        while let Some(frame) = reader.next_frame().unwrap() {
            samples += usize::from(frame.metadata.kind == Kind::Metrics);
        }
        assert_eq!(samples, 200);
    }

    #[test]
    fn an_empty_incomplete_marker_still_reports_a_reason() {
        let dir = directory();
        drop(Recorder::open(&dir, Producer::Metrics, 10000).unwrap());
        File::create(dir.join("incomplete")).unwrap();
        let reason = finalize(&dir).unwrap().incomplete.unwrap();
        assert!(reason.contains("could not be persisted"));
    }

    #[test]
    fn verification_ignores_a_forged_finalized_marker() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 10000).unwrap();
        recorder.capture(Kind::Metrics, "", b"sample");
        recorder.flush();
        // The producer is still running: no stop frame exists yet.
        std::fs::write(dir.join("finalized"), b"1\n").unwrap();
        let status = status(&dir).unwrap();
        assert!(status.finalized && status.incomplete.is_none());
        assert!(verify(&dir).unwrap().unwrap().contains("clean shutdown"));
        std::mem::forget(recorder);
    }

    #[test]
    fn reader_and_finalization_reject_truncated_payload() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 10000).unwrap();
        recorder.capture(Kind::Metrics, "", b"payload");
        drop(recorder);
        let file = OpenOptions::new()
            .write(true)
            .open(dir.join("events.bin"))
            .unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        assert!(finalize(&dir).is_err());
        assert!(!status(&dir).unwrap().finalized);
    }
    #[test]
    fn restart_preserves_committed_prefix_after_torn_append() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 10000).unwrap();
        recorder.capture(Kind::Metrics, "", b"preserved");
        drop(recorder);
        let length = committed(&dir).unwrap();
        OpenOptions::new()
            .append(true)
            .open(dir.join("events.bin"))
            .unwrap()
            .write_all(b"torn frame")
            .unwrap();
        let recorder = Recorder::open(&dir, Producer::Journal, 10000).unwrap();
        recorder.capture(Kind::Journal, "sim-test", b"must not append");
        drop(recorder);
        assert_eq!(committed(&dir).unwrap(), length);
        assert!(status(&dir).unwrap().incomplete.is_some());
        let mut reader = Reader::open(&dir).unwrap();
        let mut payloads = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            if frame.metadata.kind == Kind::Metrics {
                payloads.push(frame.payload);
            }
        }
        assert_eq!(payloads, [b"preserved".to_vec()]);
        assert!(finalize(&dir).unwrap().incomplete.is_some());
    }

    #[test]
    fn a_missing_or_emptied_recording_is_never_silently_reinitialized() {
        let dir = directory();
        drop(Recorder::open(&dir, Producer::Metrics, 10000).unwrap());
        let before = committed(&dir).unwrap();
        File::create(dir.join("events.bin")).unwrap();
        assert!(Recorder::open(&dir, Producer::Metrics, 10000).is_err());
        assert_eq!(committed(&dir).unwrap(), before);
        assert!(status(&dir).unwrap().incomplete.is_some());
    }

    #[test]
    fn writer_captures_exact_written_bytes() {
        let dir = directory();
        let recorder = Recorder::open(&dir, Producer::Metrics, 10000).unwrap();
        let mut writer = RecordedWriter::new(
            Vec::new(),
            Some(recorder.clone()),
            Kind::Metrics,
            String::new(),
        );
        writer.write_all(b"HOST_DEFINE sim-test\n").unwrap();
        writer.flush().unwrap();
        drop(writer);
        drop(recorder);
        let mut reader = Reader::open(&dir).unwrap();
        let mut output = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            if frame.metadata.kind == Kind::Metrics {
                output.extend(frame.payload);
            }
        }
        assert_eq!(output, b"HOST_DEFINE sim-test\n");
    }
}
