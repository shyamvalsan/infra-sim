//! Minimal HTTP client for the local Netdata agent.
//!
//! Hand-rolled rather than pulling in a full HTTP stack: every request is a
//! plain GET to loopback, and `spec.md`'s design note asks for a single-binary
//! console. A TLS-capable client would add a large dependency tree to fetch
//! `http://localhost:19999/api/...`.
//!
//! The console reads the agent's real state and never caches a judgement of its
//! own. If the preflight board says ML is trained, that is because the agent
//! said so — the board must not be able to show green while the product
//! disagrees.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Wall-clock cap per request. The console polls on a timer, so a hung agent
/// must not wedge the UI.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Refuse absurd responses rather than buffering without limit.
const MAX_BODY: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Agent {
    host: String,
    port: u16,
}

impl Agent {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// GET a path and parse the body as JSON.
    pub async fn get_json(&self, path: &str) -> Result<serde_json::Value, String> {
        let body = self.get(path).await?;
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON from {path}: {e}"))
    }

    /// PUT a JSON body and parse the response as JSON.
    ///
    /// Used for claiming. The body carries a credential, so it is never logged
    /// and never included in an error message.
    pub async fn put_json(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let text =
            tokio::time::timeout(TIMEOUT, self.request("PUT", path, Some(&body.to_string())))
                .await
                .map_err(|_| {
                    format!("timed out after {}s requesting {path}", TIMEOUT.as_secs())
                })??;
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON from {path}: {e}"))
    }

    async fn get(&self, path: &str) -> Result<String, String> {
        let fut = self.request("GET", path, None);
        tokio::time::timeout(TIMEOUT, fut)
            .await
            .map_err(|_| format!("timed out after {}s requesting {path}", TIMEOUT.as_secs()))?
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, String> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("cannot reach agent at {addr}: {e}"))?;

        // HTTP/1.0 so the server closes the connection when the body ends,
        // which lets read-to-EOF terminate without parsing chunked encoding.
        let payload = body.unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{payload}",
            self.host,
            payload.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;

        let mut raw = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = stream
                .read(&mut buf)
                .await
                .map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_BODY {
                return Err(format!("response from {path} exceeded {MAX_BODY} bytes"));
            }
        }

        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| "malformed HTTP response (no header terminator)".to_string())?;

        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("000");
        if !status.starts_with('2') {
            return Err(format!("agent returned HTTP {status} for {path}"));
        }
        Ok(body.to_string())
    }
}

/// Live state of one simulated node, as the agent reports it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct NodeState {
    pub hostname: String,
    pub reachable: bool,
    pub charts: usize,
    pub contexts: usize,
    pub alarms_total: usize,
    pub alarms_warning: usize,
    pub alarms_critical: usize,
    /// Dimensions the ML engine has finished training.
    pub ml_trained: f64,
    pub ml_untrained: f64,
    pub anomaly_rate: f64,
}

impl NodeState {
    /// Fraction of dimensions with a trained model.
    pub fn ml_fraction(&self) -> f64 {
        let total = self.ml_trained + self.ml_untrained;
        if total <= 0.0 {
            0.0
        } else {
            self.ml_trained / total
        }
    }
}

impl Agent {
    /// Hostnames the agent knows about, including virtual nodes.
    pub async fn nodes(&self) -> Result<Vec<String>, String> {
        let v = self.get_json("/api/v3/nodes").await?;
        Ok(v.get("nodes")
            .and_then(|n| n.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.get("nm").and_then(|x| x.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The agent's own machine GUID, needed to address the per-node ML charts
    /// (the agent suffixes them with the parent's GUID).
    pub async fn machine_guid(&self) -> Result<String, String> {
        let v = self.get_json("/api/v1/info").await?;
        v.get("uid")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "agent info has no uid".to_string())
    }

    pub async fn node_state(&self, hostname: &str, agent_guid: &str) -> NodeState {
        let mut st = NodeState {
            hostname: hostname.to_string(),
            ..Default::default()
        };

        if let Ok(v) = self
            .get_json(&format!("/host/{hostname}/api/v1/charts"))
            .await
        {
            if let Some(charts) = v.get("charts").and_then(|c| c.as_object()) {
                st.reachable = true;
                st.charts = charts.len();
                let mut ctx: std::collections::BTreeSet<&str> = Default::default();
                for c in charts.values() {
                    if let Some(name) = c.get("context").and_then(|x| x.as_str()) {
                        ctx.insert(name);
                    }
                }
                st.contexts = ctx.len();
            }
        }

        if let Ok(v) = self
            .get_json(&format!("/host/{hostname}/api/v1/alarms?active=true"))
            .await
        {
            if let Some(alarms) = v.get("alarms").and_then(|a| a.as_object()) {
                st.alarms_total = alarms.len();
                for a in alarms.values() {
                    match a.get("status").and_then(|x| x.as_str()) {
                        Some("WARNING") => st.alarms_warning += 1,
                        Some("CRITICAL") => st.alarms_critical += 1,
                        _ => {}
                    }
                }
            }
        }

        let training = format!(
            "/host/{hostname}/api/v1/data?chart=netdata.training_status_on_{agent_guid}\
             &after=-10&points=1&format=json"
        );
        if let Ok(v) = self.get_json(&training).await {
            if let Some((labels, row)) = first_row(&v) {
                for (i, l) in labels.iter().enumerate() {
                    let val = row.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0);
                    match l.as_str() {
                        "trained" => st.ml_trained = val,
                        "untrained" | "pending-without-model" => st.ml_untrained += val,
                        _ => {}
                    }
                }
            }
        }

        let ar = format!(
            "/host/{hostname}/api/v1/data?chart=anomaly_detection.anomaly_rate_on_{agent_guid}\
             &after=-60&points=1&format=json"
        );
        if let Ok(v) = self.get_json(&ar).await {
            if let Some((_, row)) = first_row(&v) {
                st.anomaly_rate = row.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0);
            }
        }

        st
    }
}

/// Labels and the first data row of a Netdata `/api/v1/data` response.
fn first_row(v: &serde_json::Value) -> Option<(Vec<String>, Vec<serde_json::Value>)> {
    let labels = v
        .get("labels")?
        .as_array()?
        .iter()
        .map(|l| l.as_str().unwrap_or_default().to_string())
        .collect();
    let row = v.get("data")?.as_array()?.first()?.as_array()?.clone();
    Some((labels, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ml_fraction_handles_the_no_data_case() {
        let st = NodeState::default();
        assert_eq!(st.ml_fraction(), 0.0);
    }

    #[test]
    fn ml_fraction_computes_progress() {
        let st = NodeState {
            ml_trained: 25.0,
            ml_untrained: 75.0,
            ..Default::default()
        };
        assert!((st.ml_fraction() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn first_row_extracts_labels_and_values() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"labels":["time","trained","untrained"],"data":[[100,25,75]]}"#,
        )
        .unwrap();
        let (labels, row) = first_row(&v).unwrap();
        assert_eq!(labels, vec!["time", "trained", "untrained"]);
        assert_eq!(row[1].as_f64(), Some(25.0));
    }

    #[test]
    fn first_row_returns_none_on_an_empty_result() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"labels":["time"],"data":[]}"#).unwrap();
        assert!(first_row(&v).is_none());
    }

    #[tokio::test]
    async fn an_unreachable_agent_reports_an_error_rather_than_hanging() {
        // Port 1 is reserved and never listening.
        let a = Agent::new("127.0.0.1", 1);
        let err = a.get_json("/api/v1/info").await.unwrap_err();
        assert!(err.contains("cannot reach agent"), "{err}");
    }
}
