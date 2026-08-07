//! The `environment.yaml` contract.
//!
//! `spec.md` promises that `environment.yaml` plus a seed reproduces a world
//! bit-for-bit. That makes this file, together with the generator spec it
//! references, the complete definition of a simulated fleet — nothing about a
//! demo may live outside these two artifacts if archive-and-replay is to mean
//! anything.
//!
//! Claim tokens and room IDs are deliberately absent. They are credentials,
//! they are supplied at runtime, and they must never reach a committed or
//! archived file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sim_engine::{Instance, NodeProfile};
use thiserror::Error;

pub const ENVIRONMENT_VERSION: u32 = 1;

/// Host label marking every simulated node. `spec.md` requires this on
/// everything, and the console enforces it; the runtime applies it
/// unconditionally so a hand-edited environment cannot ship unlabelled.
pub const SIMULATED_LABEL: (&str, &str) = ("simulated", "true");

#[derive(Debug, Error)]
pub enum EnvError {
    #[error("failed to read environment '{path}': {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse environment '{path}': {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error(
        "unsupported environment version {found}, this build understands {ENVIRONMENT_VERSION}"
    )]
    Version { found: u32 },

    #[error("environment '{name}' defines no nodes")]
    NoNodes { name: String },

    #[error("duplicate node GUID '{guid}' - GUIDs are node identity and must be unique")]
    DuplicateGuid { guid: String },

    #[error("duplicate hostname '{hostname}'")]
    DuplicateHostname { hostname: String },

    #[error("node '{hostname}' has GUID '{guid}', which is not a valid UUID")]
    MalformedGuid { hostname: String, guid: String },

    #[error("update_every must be >= 1, got {found}")]
    BadUpdateEvery { found: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,

    /// Master seed. With the generator spec and this file, it fully determines
    /// the emitted stream.
    pub seed: u64,

    #[serde(default = "default_update_every")]
    pub update_every: i64,

    /// Generator spec path, resolved relative to this file's directory so an
    /// archived environment stays self-contained.
    pub generator: PathBuf,

    /// Scenario directory, resolved the same way. Defaults to `scenarios`
    /// beside the environment file, which is the installed layout; a repo
    /// checkout points it at the shared `scenarios/` directory so the files are
    /// not duplicated per environment.
    #[serde(default = "default_scenario_dir")]
    pub scenarios: PathBuf,

    pub nodes: Vec<NodeDef>,
}

fn default_update_every() -> i64 {
    1
}

fn default_scenario_dir() -> PathBuf {
    PathBuf::from("scenarios")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDef {
    pub hostname: String,
    /// Stable UUID identifying the node to Netdata. Changing it orphans the
    /// node's history; changing `hostname` alone renames in place. The re-skin
    /// workflow depends on that, so GUIDs are authored, never generated per run.
    pub guid: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub utc_offset_secs: i64,
    #[serde(default)]
    pub attrs: BTreeMap<String, f64>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Per-device resources keyed by group: `disk`, `net`, `mount`. Contexts
    /// declaring instancing expand to one chart per entry. A node omitting a
    /// group emits no charts for it, exactly as a host without that hardware
    /// would.
    #[serde(default)]
    pub instances: BTreeMap<String, Vec<InstanceDef>>,
}

/// One device. Accepts either a bare name or a table, so the common case stays
/// terse: `disk: [nvme0n1, sdb]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InstanceDef {
    Name(String),
    Detailed {
        name: String,
        /// Scales every signal for this device. A busy root disk and a nearly
        /// idle secondary are the same spec at different weights.
        #[serde(default = "default_weight")]
        weight: f64,
        /// Attributes shadowing the node's, e.g. a per-mount `disk_total_kb`.
        #[serde(default)]
        attrs: BTreeMap<String, f64>,
    },
}

fn default_weight() -> f64 {
    1.0
}

impl InstanceDef {
    pub fn name(&self) -> &str {
        match self {
            InstanceDef::Name(n) => n,
            InstanceDef::Detailed { name, .. } => name,
        }
    }

    fn to_instance(&self) -> Instance {
        match self {
            InstanceDef::Name(n) => Instance {
                name: n.clone(),
                weight: 1.0,
                attrs: BTreeMap::new(),
            },
            InstanceDef::Detailed {
                name,
                weight,
                attrs,
            } => Instance {
                name: name.clone(),
                weight: *weight,
                attrs: attrs.clone(),
            },
        }
    }
}

impl Environment {
    pub fn load(path: &Path) -> Result<Self, EnvError> {
        let raw = std::fs::read_to_string(path).map_err(|source| EnvError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let env: Environment = serde_yaml::from_str(&raw).map_err(|source| EnvError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        env.validate()?;
        Ok(env)
    }

    /// Generator spec path resolved against the environment file's directory.
    pub fn generator_path(&self, env_path: &Path) -> PathBuf {
        resolve(&self.generator, env_path)
    }

    /// Scenario directory resolved against the environment file's directory.
    pub fn scenario_path(&self, env_path: &Path) -> PathBuf {
        resolve(&self.scenarios, env_path)
    }

    fn validate(&self) -> Result<(), EnvError> {
        if self.version != ENVIRONMENT_VERSION {
            return Err(EnvError::Version {
                found: self.version,
            });
        }
        if self.nodes.is_empty() {
            return Err(EnvError::NoNodes {
                name: self.name.clone(),
            });
        }
        if self.update_every < 1 {
            return Err(EnvError::BadUpdateEvery {
                found: self.update_every,
            });
        }

        let mut guids = BTreeMap::new();
        let mut hostnames = BTreeMap::new();
        for node in &self.nodes {
            if !is_uuid(&node.guid) {
                return Err(EnvError::MalformedGuid {
                    hostname: node.hostname.clone(),
                    guid: node.guid.clone(),
                });
            }
            if guids.insert(node.guid.clone(), ()).is_some() {
                return Err(EnvError::DuplicateGuid {
                    guid: node.guid.clone(),
                });
            }
            if hostnames.insert(node.hostname.clone(), ()).is_some() {
                return Err(EnvError::DuplicateHostname {
                    hostname: node.hostname.clone(),
                });
            }
        }
        Ok(())
    }

    /// Node profiles for the engine, with the mandatory simulation label
    /// applied. Labels in the environment cannot override it.
    pub fn profiles(&self) -> Vec<NodeProfile> {
        self.nodes
            .iter()
            .map(|n| {
                let mut labels = n.labels.clone();
                labels.insert(SIMULATED_LABEL.0.to_string(), SIMULATED_LABEL.1.to_string());
                let instances = n
                    .instances
                    .iter()
                    .map(|(group, defs)| {
                        (
                            group.clone(),
                            defs.iter().map(|d| d.to_instance()).collect(),
                        )
                    })
                    .collect();
                NodeProfile {
                    guid: n.guid.clone(),
                    hostname: n.hostname.clone(),
                    role: n.role.clone(),
                    attrs: n.attrs.clone(),
                    labels,
                    instances,
                    utc_offset_secs: n.utc_offset_secs,
                }
            })
            .collect()
    }
}

fn resolve(path: &Path, env_path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

/// Netdata parses the machine GUID with `uuid_parse`, so anything that is not
/// canonical 8-4-4-4-12 hex disables the plugin at runtime. Rejecting it here
/// turns a silent production failure into a startup error.
fn is_uuid(s: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != groups.len() {
        return false;
    }
    parts
        .iter()
        .zip(groups)
        .all(|(p, want)| p.len() == want && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: &str = r#"
version: 1
name: test-env
seed: 12345
update_every: 1
generator: specs/linux-system.yaml
nodes:
  - hostname: sim-web-01
    guid: 9c6b1e42-7a3f-4d18-9e57-2b40c8f1a601
    role: web
    attrs: { cores: 8, ram_total_kb: 16777216 }
    labels: { infra_sim_role: web }
"#;

    fn parse(yaml: &str) -> Result<Environment, EnvError> {
        let env: Environment = serde_yaml::from_str(yaml).map_err(|source| EnvError::Parse {
            path: PathBuf::from("test"),
            source,
        })?;
        env.validate()?;
        Ok(env)
    }

    #[test]
    fn accepts_a_wellformed_environment() {
        let env = parse(ENV).expect("should parse");
        assert_eq!(env.nodes.len(), 1);
        assert_eq!(env.seed, 12345);
    }

    #[test]
    fn simulated_label_is_always_applied() {
        let env = parse(ENV).unwrap();
        let p = &env.profiles()[0];
        assert_eq!(p.labels.get("simulated").map(String::as_str), Some("true"));
    }

    #[test]
    fn simulated_label_cannot_be_overridden_by_the_environment() {
        let yaml = ENV.replace("{ infra_sim_role: web }", "{ simulated: \"false\" }");
        let env = parse(&yaml).unwrap();
        let p = &env.profiles()[0];
        assert_eq!(p.labels.get("simulated").map(String::as_str), Some("true"));
    }

    #[test]
    fn rejects_malformed_guid() {
        let yaml = ENV.replace("9c6b1e42-7a3f-4d18-9e57-2b40c8f1a601", "not-a-uuid");
        let err = parse(&yaml).unwrap_err();
        assert!(err.to_string().contains("not a valid UUID"), "{err}");
    }

    #[test]
    fn rejects_duplicate_guids() {
        let yaml = format!(
            "{ENV}  - hostname: sim-web-02\n    guid: 9c6b1e42-7a3f-4d18-9e57-2b40c8f1a601\n"
        );
        let err = parse(&yaml).unwrap_err();
        assert!(err.to_string().contains("duplicate node GUID"), "{err}");
    }

    #[test]
    fn rejects_duplicate_hostnames() {
        let yaml = format!(
            "{ENV}  - hostname: sim-web-01\n    guid: 3d81f570-6c24-4a9b-8f13-7e55ab29d4c2\n"
        );
        let err = parse(&yaml).unwrap_err();
        assert!(err.to_string().contains("duplicate hostname"), "{err}");
    }
}
