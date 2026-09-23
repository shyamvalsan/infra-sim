//! Re-inject recorded producer bytes; downstream Netdata results remain live.
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use prost::{
    bytes::{Buf, BufMut},
    Message,
};
use sim_engine::recording::{self, Frame, Kind, Reader};

struct Stream {
    reader: Reader,
    origin: u64,
    started: Instant,
    start_offset_ns: i128,
}

impl Stream {
    fn open(dir: &Path, allow_incomplete: bool, start_at: Option<u64>) -> Result<Self, String> {
        recording::verify_archive_inventory(dir)?;
        let status = recording::status(dir).map_err(|e| e.to_string())?;
        if !status.finalized {
            return Err("stop recording producers and finalize the recording before replay".into());
        }
        // Validates the whole bounded stream before any output, and re-derives
        // session completeness rather than trusting marker files.
        let verified = recording::verify(dir).map_err(|e| e.to_string())?;
        if let Some(reason) = status.incomplete.or(verified.map(String::from)) {
            if !allow_incomplete {
                return Err(format!("recording is incomplete: {reason}; use --allow-incomplete-recording to replay only its committed prefix"));
            }
            eprintln!(
                "infra-sim replay: incomplete recording, replaying its committed prefix: {reason}"
            );
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos() as i128;
        Ok(Self {
            start_offset_ns: start_at.map_or(0, |start| i128::from(start) - now_ns),
            reader: Reader::open(dir).map_err(|e| e.to_string())?,
            origin: recording::manifest(dir)
                .map_err(|e| e.to_string())?
                .origin_ns,
            started: Instant::now(),
        })
    }

    fn next(&mut self, kinds: &[Kind]) -> Result<Option<Frame>, String> {
        while let Some(frame) = self.reader.next_frame().map_err(|e| e.to_string())? {
            if kinds.contains(&frame.metadata.kind) {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }

    fn remaining(&self, frame: &Frame) -> Duration {
        let remaining = self.start_offset_ns
            + i128::from(frame.metadata.observed_ns.saturating_sub(self.origin))
            - self.started.elapsed().as_nanos() as i128;
        Duration::from_nanos(remaining.clamp(0, i128::from(u64::MAX)) as u64)
    }

    fn pace(&self, frame: &Frame) -> Result<(), String> {
        while !self.remaining(frame).is_zero() {
            interrupted()?;
            std::thread::sleep(self.remaining(frame).min(Duration::from_millis(100)));
        }
        interrupted()
    }
}

fn interrupted() -> Result<(), String> {
    if crate::shutdown::requested() {
        Err("replay interrupted before completion".into())
    } else {
        Ok(())
    }
}

pub fn run(dir: &Path, args: &crate::Args) -> Result<(), String> {
    crate::shutdown::install()?;
    if [args.logs, args.otlp, args.exporters]
        .iter()
        .filter(|v| **v)
        .count()
        > 1
    {
        return Err("choose one replay producer mode per process".into());
    }
    let stream = Stream::open(dir, args.allow_incomplete_recording, args.replay_start_at)?;
    if args.otlp {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        return runtime.block_on(otlp(stream, &args.otlp_endpoint));
    }
    if args.logs {
        return journals(stream, args);
    }
    if args.exporters {
        return exporters(stream, args.exporter_port);
    }
    metrics(stream, &mut std::io::stdout().lock())
}

fn metrics(mut stream: Stream, output: &mut impl Write) -> Result<(), String> {
    while let Some(frame) = stream.next(&[Kind::Metrics])? {
        stream.pace(&frame)?;
        output
            .write_all(&frame.payload)
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct Journal {
    child: Child,
    input: Option<ChildStdin>,
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.input.take();
        // Only this owned receiver is eligible for termination.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
    }
}

fn journal_target(target: &str) -> Result<(), String> {
    if !target.starts_with("remote-")
        || !target.ends_with(".journal")
        || !target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || target.len() > 240
    {
        return Err("invalid recorded journal filename".into());
    }
    Ok(())
}

fn journals(mut stream: Stream, args: &crate::Args) -> Result<(), String> {
    let remote = crate::logs_runtime::find_journal_remote(args.journal_remote.as_deref())?;
    std::fs::create_dir_all(&args.journal_dir).map_err(|e| e.to_string())?;
    let mut receivers: BTreeMap<String, Journal> = BTreeMap::new();
    while let Some(frame) = stream.next(&[Kind::Journal])? {
        journal_target(&frame.metadata.target)?;
        if !receivers.contains_key(&frame.metadata.target) {
            if receivers.len() >= crate::logs_runtime::MAX_JOURNAL_PROCESSES {
                return Err("recording exceeds the journal receiver limit".into());
            }
            let path = args.journal_dir.join(&frame.metadata.target);
            let mut child = Command::new(&remote)
                .arg(format!("--output={}", path.display()))
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?;
            let input = child.stdin.take();
            receivers.insert(frame.metadata.target.clone(), Journal { child, input });
        }
        stream.pace(&frame)?;
        receivers
            .get_mut(&frame.metadata.target)
            .unwrap()
            .input
            .as_mut()
            .ok_or("journal receiver stdin unavailable")?
            .write_all(&frame.payload)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

const RECEIVER_STARTUP: Duration = Duration::from_secs(60);

/// Tonic adds only the gRPC envelope; protobuf bytes are never reconstructed.
struct RawCodec;
impl tonic::codec::Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = Self;
    type Decoder = Self;
    fn encoder(&mut self) -> Self {
        Self
    }
    fn decoder(&mut self) -> Self {
        Self
    }
}
impl tonic::codec::Encoder for RawCodec {
    type Item = Vec<u8>;
    type Error = tonic::Status;
    fn encode(
        &mut self,
        bytes: Vec<u8>,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        dst.put_slice(&bytes);
        Ok(())
    }
}
impl tonic::codec::Decoder for RawCodec {
    type Item = Vec<u8>;
    type Error = tonic::Status;
    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(Some(src.copy_to_bytes(src.remaining()).to_vec()))
    }
}

async fn otlp(mut stream: Stream, endpoint: &str) -> Result<(), String> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceResponse;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
    let channel = tonic::transport::Channel::from_shared(format!("http://{endpoint}"))
        .map_err(|e| e.to_string())?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .connect_lazy();
    let mut client = tonic::client::Grpc::new(channel);
    // A receiver may still be starting when replay begins. Retry only until the
    // first acknowledgement: afterwards a retry could duplicate delivered data.
    let mut acknowledged = false;
    while let Some(frame) = stream.next(&[Kind::OtlpLogs, Kind::OtlpTraces])? {
        while !stream.remaining(&frame).is_zero() {
            interrupted()?;
            tokio::time::sleep(stream.remaining(&frame).min(Duration::from_millis(100))).await;
        }
        interrupted()?;
        let path = match frame.metadata.kind {
            Kind::OtlpLogs => "/opentelemetry.proto.collector.logs.v1.LogsService/Export",
            Kind::OtlpTraces => "/opentelemetry.proto.collector.trace.v1.TraceService/Export",
            _ => unreachable!(),
        };
        let deadline = Instant::now() + RECEIVER_STARTUP;
        let response = loop {
            client.ready().await.map_err(|e| e.to_string())?;
            let result = client
                .unary(
                    tonic::Request::new(frame.payload.clone()),
                    tonic::codegen::http::uri::PathAndQuery::from_static(path),
                    RawCodec,
                )
                .await;
            match result {
                Ok(response) => break response.into_inner(),
                Err(status)
                    if !acknowledged
                        && status.code() == tonic::Code::Unavailable
                        && Instant::now() < deadline =>
                {
                    interrupted()?;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(status) => return Err(format!("OTLP replay export failed: {status}")),
            }
        };
        acknowledged = true;
        let rejected = match frame.metadata.kind {
            Kind::OtlpLogs => ExportLogsServiceResponse::decode(response.as_slice())
                .map_err(|e| e.to_string())?
                .partial_success
                .map(|p| p.rejected_log_records)
                .unwrap_or(0),
            Kind::OtlpTraces => ExportTraceServiceResponse::decode(response.as_slice())
                .map_err(|e| e.to_string())?
                .partial_success
                .map(|p| p.rejected_spans)
                .unwrap_or(0),
            _ => unreachable!(),
        };
        if rejected != 0 {
            return Err(format!(
                "OTLP receiver rejected {rejected} replayed records"
            ));
        }
    }
    Ok(())
}

fn exporters(mut stream: Stream, port: u16) -> Result<(), String> {
    // Index offsets only; keep raw response bodies on disk. Each route advances
    // independently when scraped and retains its own recorded response order.
    let mut routes: BTreeMap<String, VecDeque<u64>> = BTreeMap::new();
    loop {
        let offset = stream.reader.position().map_err(|e| e.to_string())?;
        let Some(frame) = stream.reader.next_frame().map_err(|e| e.to_string())? else {
            break;
        };
        if frame.metadata.kind != Kind::Exporter {
            continue;
        }
        let target = &frame.metadata.target;
        if !target.starts_with("/metrics/")
            || target.len() > 1024
            || target
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err("invalid recorded exporter route".into());
        }
        routes.entry(target.clone()).or_default().push_back(offset);
    }
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    while routes.values().any(|frames| !frames.is_empty()) {
        interrupted()?;
        let mut client = match listener.accept() {
            Ok((client, _)) => client,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        client.set_nonblocking(false).map_err(|e| e.to_string())?;
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new((&client).take(8192));
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let route = line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("")
            .to_owned();
        let mut complete = false;
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line == "\r\n" || line == "\n" => {
                    complete = true;
                    break;
                }
                _ => {}
            }
        }
        if !complete {
            continue;
        }
        let Some(offset) = routes
            .get(&route)
            .and_then(|frames| frames.front())
            .copied()
        else {
            let _ = client
                .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        };
        stream
            .reader
            .seek_frame(offset)
            .map_err(|e| e.to_string())?;
        let frame = stream
            .reader
            .next_frame()
            .map_err(|e| e.to_string())?
            .ok_or("recorded exporter frame missing")?;
        stream.pace(&frame)?;
        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", frame.payload.len());
        client
            .write_all(header.as_bytes())
            .and_then(|_| client.write_all(&frame.payload))
            .map_err(|e| e.to_string())?;
        routes.get_mut(&route).unwrap().pop_front();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_targets_cannot_escape_output_directory() {
        for invalid in [
            "../remote-sim.journal",
            "remote-../../sim.journal",
            "remote-/sim.journal",
            "remote-sim.journal\n",
        ] {
            assert!(journal_target(invalid).is_err());
        }
        assert!(journal_target("remote-sim-db-01.journal").is_ok());
        assert!(journal_target("remote-shard-01.journal").is_ok());
    }
}
