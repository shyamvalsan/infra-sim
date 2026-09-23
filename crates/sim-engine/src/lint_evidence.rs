//! Portable input fingerprints for evidence from an actual lint invocation.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FILENAME: &str = "lint-evidence.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub version: u32,
    pub finished_at: u64,
    pub hours: i64,
    pub passed: bool,
    pub inputs: Inputs,
    pub executable_sha256: String,
    pub runtime_image: Option<String>,
}

pub fn file_hash(path: &Path) -> Result<String, String> {
    let mut source = File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn tree(
    root: &Path,
    path: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("lint evidence does not follow symlinked input trees".into());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            tree(
                root,
                &entry.map_err(|e| e.to_string())?.path(),
                prefix,
                files,
            )?;
        }
    } else if metadata.is_file() {
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        files.insert(
            format!("{prefix}/{}", relative.to_string_lossy().replace('\\', "/")),
            file_hash(path)?,
        );
    } else {
        return Err("lint inputs must be regular files or directories".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct References {
    generator: PathBuf,
    #[serde(default)]
    specs: Option<PathBuf>,
    #[serde(default)]
    scenarios: Option<PathBuf>,
    nodes: Vec<NodeReference>,
}
#[derive(Deserialize)]
struct NodeReference {
    #[serde(default)]
    generator: Option<PathBuf>,
}

pub fn inputs(environment: &Path) -> Result<Inputs, String> {
    // Read the path references from the same bytes being fingerprinted. Full
    // environment validation remains the runtime's responsibility.
    let raw = std::fs::read(environment).map_err(|e| e.to_string())?;
    let refs: References = serde_yaml::from_slice(&raw).map_err(|e| e.to_string())?;
    let parent = environment.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            parent.join(path)
        }
    };
    let mut hashes = BTreeMap::new();
    let mut cached_hash = |path: &Path| -> Result<String, String> {
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        if let Some(hash) = hashes.get(&path) {
            return Ok(String::clone(hash));
        }
        let hash = file_hash(&path)?;
        hashes.insert(path, hash.clone());
        Ok(hash)
    };
    let mut files = BTreeMap::new();
    files.insert("environment".into(), format!("{:x}", Sha256::digest(&raw)));
    files.insert("generator".into(), cached_hash(&resolve(&refs.generator))?);
    for (index, node) in refs.nodes.iter().enumerate() {
        files.insert(
            format!("node/{index}/generator"),
            cached_hash(&resolve(
                node.generator.as_deref().unwrap_or(&refs.generator),
            ))?,
        );
    }
    for (prefix, root) in [
        (
            "specs",
            refs.specs.unwrap_or_else(|| PathBuf::from("specs")),
        ),
        (
            "scenarios",
            refs.scenarios.unwrap_or_else(|| PathBuf::from("scenarios")),
        ),
    ] {
        let root = resolve(&root);
        if root.exists() {
            tree(&root, &root, prefix, &mut files)?;
        } else {
            files.insert(prefix.into(), "missing".into());
        }
    }
    Ok(Inputs { files })
}

pub fn write(
    path: &Path,
    inputs: Inputs,
    executable: &Path,
    runtime_image: Option<String>,
    hours: i64,
    passed: bool,
) -> Result<(), String> {
    let evidence = Evidence {
        version: 1,
        finished_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs(),
        hours,
        passed,
        inputs,
        runtime_image,
        executable_sha256: file_hash(executable)?,
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".lint-evidence-{}.tmp", std::process::id()));
    let mut file = File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

/// The caller supplies the selected runtime's identity, never a global flag.
pub fn verified(
    environment: &Path,
    executable: Option<&Path>,
    image: Option<&str>,
) -> Result<Evidence, String> {
    let path = environment
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FILENAME);
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("lint evidence is too large".into());
    }
    let evidence: Evidence = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if evidence.version != 1 || evidence.hours < 1 || evidence.finished_at == 0 {
        return Err("unsupported or invalid lint evidence".into());
    }
    if evidence.inputs != inputs(environment)? {
        return Err("lint evidence is stale: inputs changed".into());
    }
    match (executable, image) {
        (_, Some(image))
            if !image.is_empty() && evidence.runtime_image.as_deref() == Some(image) => {}
        (Some(executable), None)
            if evidence.runtime_image.is_none()
                && file_hash(executable)? == evidence.executable_sha256 => {}
        _ => return Err("lint evidence is stale: runtime identity changed or unavailable".into()),
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removed on drop so repeated test runs cannot fill a small tmpfs.
    struct Fixture(PathBuf);
    impl std::ops::Deref for Fixture {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> Fixture {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("infra-sim-evidence-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(dir.join("specs")).unwrap();
        std::fs::create_dir_all(dir.join("scenarios")).unwrap();
        std::fs::write(
            dir.join("environment.yaml"),
            "generator: specs/base.yaml\nnodes:\n  - generator: specs/base.yaml\n",
        )
        .unwrap();
        std::fs::write(dir.join("specs/base.yaml"), "synthetic generator").unwrap();
        std::fs::write(dir.join("scenarios/fault.yaml"), "synthetic scenario").unwrap();
        std::fs::write(dir.join("binary"), "synthetic executable identity").unwrap();
        Fixture(dir)
    }

    #[test]
    fn changed_inputs_and_executable_invalidate_prior_pass() {
        let dir = fixture();
        let env = dir.join("environment.yaml");
        let executable = dir.join("binary");
        write(
            &dir.join(FILENAME),
            inputs(&env).unwrap(),
            &executable,
            None,
            2,
            true,
        )
        .unwrap();
        assert!(verified(&env, Some(&executable), None).unwrap().passed);
        for name in [
            "environment.yaml",
            "specs/base.yaml",
            "scenarios/fault.yaml",
            "binary",
        ] {
            let path = dir.join(name);
            let original = std::fs::read(&path).unwrap();
            let mut changed = original.clone();
            changed.extend_from_slice(b"\n# changed\n");
            std::fs::write(&path, changed).unwrap();
            assert!(verified(&env, Some(&executable), None).is_err(), "{name}");
            std::fs::write(path, original).unwrap();
        }
        assert!(verified(&env, Some(&executable), None).unwrap().passed);
    }

    #[test]
    fn copied_payload_remains_identical_but_another_runtime_does_not() {
        let original = fixture();
        let copied = fixture();
        let source = original.join("environment.yaml");
        let target = copied.join("environment.yaml");
        assert_eq!(inputs(&source).unwrap(), inputs(&target).unwrap());
        write(
            &copied.join(FILENAME),
            inputs(&source).unwrap(),
            &original.join("binary"),
            Some("sha256:synthetic-image-a".into()),
            2,
            false,
        )
        .unwrap();
        assert!(
            !verified(&target, None, Some("sha256:synthetic-image-a"))
                .unwrap()
                .passed
        );
        assert!(verified(&target, None, Some("sha256:synthetic-image-b")).is_err());
        assert!(verified(&target, Some(&copied.join("binary")), None).is_err());
    }

    #[test]
    fn adding_an_input_invalidates_evidence() {
        let dir = fixture();
        let env = dir.join("environment.yaml");
        let before = inputs(&env).unwrap();
        std::fs::write(dir.join("scenarios/another.yaml"), "another scenario").unwrap();
        assert_ne!(before, inputs(&env).unwrap());
    }
    #[test]
    fn sha256_uses_standard_content_identity() {
        let dir = std::env::temp_dir().join(format!("infra-sim-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("input");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_hash(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
