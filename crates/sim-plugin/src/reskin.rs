//! Re-skinning a warm environment for a new prospect.
//!
//! `spec.md` §2: keep base environments running warm, and for a new prospect
//! rewrite hostnames and labels to match their conventions. The environment
//! keeps its trained ML models, retention and alert history, turning a cold
//! 72-hour start into a change measured in minutes.
//!
//! The whole workflow rests on one distinction: **the GUID is the identity.**
//! Change it and Netdata treats the node as new and orphans its history;
//! change only the hostname and the node is renamed in place with everything
//! intact. So re-skinning must never touch a GUID, and this module refuses to
//! emit an environment where one changed.
//!
//! It also refuses to *duplicate* GUIDs across environments, because two live
//! fleets sharing a GUID is the move-don't-clone rule being broken — the second
//! claim silently takes over the first node's identity.

use std::collections::BTreeMap;
use std::path::Path;

/// How to re-skin an environment.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Hostname prefix to replace, e.g. `sim-`.
    pub from_prefix: String,
    /// Replacement prefix, e.g. `acme-`.
    pub to_prefix: String,
    /// New environment name.
    pub name: Option<String>,
    /// Labels applied to every node, overriding existing values.
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct Outcome {
    pub renamed: Vec<(String, String)>,
    pub yaml: String,
}

/// Rewrite an environment's hostnames and labels, leaving GUIDs untouched.
///
/// Operates on the YAML text rather than parsing and re-serialising, so
/// comments and formatting survive. An environment file carries a lot of
/// authored explanation — why a GUID must not change, why a mount has the size
/// it has — and re-serialising would discard exactly the notes the next person
/// needs.
pub fn reskin(source: &str, plan: &Plan) -> Result<Outcome, String> {
    if plan.from_prefix.is_empty() || plan.to_prefix.is_empty() {
        return Err("both --from-prefix and --to-prefix are required".into());
    }

    let before_guids = guids(source);
    if before_guids.is_empty() {
        return Err("no node GUIDs found; is this an environment file?".into());
    }

    let mut renamed = Vec::new();
    let mut out = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("- hostname:") {
            let old = rest.trim();
            if let Some(tail) = old.strip_prefix(plan.from_prefix.as_str()) {
                let new = format!("{}{}", plan.to_prefix, tail);
                renamed.push((old.to_string(), new.clone()));
                let indent = &line[..line.len() - trimmed.len()];
                out.push(format!("{indent}- hostname: {new}"));
                continue;
            }
        }

        if let Some(rest) = trimmed.strip_prefix("name:") {
            // The environment's own name, not a node's.
            if !line.starts_with(' ') {
                if let Some(new_name) = &plan.name {
                    let _ = rest;
                    out.push(format!("name: {new_name}"));
                    continue;
                }
            }
        }

        // Label overrides are applied in place so their position and any
        // surrounding comments are preserved.
        let mut replaced = false;
        for (key, value) in &plan.labels {
            if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
                let _ = rest;
                let indent = &line[..line.len() - trimmed.len()];
                out.push(format!("{indent}{key}: {value}"));
                replaced = true;
                break;
            }
        }
        if replaced {
            continue;
        }

        out.push(line.to_string());
    }

    let yaml = out.join("\n") + "\n";

    // The load-bearing check. A re-skin that changed a GUID would look like a
    // success and silently orphan every node's history.
    let after_guids = guids(&yaml);
    if before_guids != after_guids {
        return Err(
            "re-skin would change node GUIDs, which orphans history - refusing to write".into(),
        );
    }

    if renamed.is_empty() {
        return Err(format!(
            "no hostnames start with '{}'; nothing to re-skin",
            plan.from_prefix
        ));
    }

    Ok(Outcome { renamed, yaml })
}

fn guids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix("guid:"))
        .map(|g| g.trim().to_string())
        .collect()
}

/// Refuse to write an environment whose GUIDs already appear in another
/// environment beside it.
///
/// Two environments sharing a GUID cannot both be claimed: Netdata treats them
/// as the same node, so the second claim takes over the first's identity. The
/// console enforces one active instance per base; this is the same rule applied
/// at the point a new environment is created.
pub fn check_guid_uniqueness(dir: &Path, new_yaml: &str, self_path: &Path) -> Result<(), String> {
    let mine = guids(new_yaml);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") || path == self_path {
            continue;
        }
        let Ok(other) = std::fs::read_to_string(&path) else {
            continue;
        };
        let theirs = guids(&other);
        let clash: Vec<&String> = mine.iter().filter(|g| theirs.contains(g)).collect();
        if !clash.is_empty() {
            return Err(format!(
                "GUID(s) {} already used by '{}'. Two environments sharing a GUID cannot both \
                 be claimed - the second claim takes over the first node's identity. Re-skin \
                 that environment instead of cloning it, or regenerate GUIDs and accept a cold \
                 72h warm-up.",
                clash
                    .iter()
                    .map(|g| g.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: &str = r#"version: 1
name: web-stack
seed: 8675309
generator: ../specs/linux-system.yaml
nodes:

  - hostname: sim-lb-01
    guid: 4a1f9d20-5e83-4c17-b6a2-0d94e7fc3518
    role: lb
    labels:
      infra_sim_env: production
      _cloud_instance_region: us-east-1

  - hostname: sim-db-01
    guid: 3d81f570-6c24-4a9b-8f13-7e55ab29d4c2
    role: db
    labels:
      infra_sim_env: production
      _cloud_instance_region: us-east-1
"#;

    fn plan() -> Plan {
        Plan {
            from_prefix: "sim-".into(),
            to_prefix: "acme-".into(),
            name: Some("acme-prod".into()),
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn renames_hostnames_and_preserves_guids() {
        let out = reskin(ENV, &plan()).expect("re-skins");
        assert!(out.yaml.contains("hostname: acme-lb-01"));
        assert!(out.yaml.contains("hostname: acme-db-01"));
        // The whole workflow depends on these surviving untouched.
        assert!(out
            .yaml
            .contains("guid: 4a1f9d20-5e83-4c17-b6a2-0d94e7fc3518"));
        assert!(out
            .yaml
            .contains("guid: 3d81f570-6c24-4a9b-8f13-7e55ab29d4c2"));
        assert_eq!(out.renamed.len(), 2);
    }

    #[test]
    fn renames_the_environment() {
        let out = reskin(ENV, &plan()).unwrap();
        assert!(out.yaml.contains("name: acme-prod"));
    }

    #[test]
    fn applies_label_overrides_in_place() {
        let mut p = plan();
        p.labels
            .insert("_cloud_instance_region".into(), "eu-central-1".into());
        let out = reskin(ENV, &p).unwrap();
        assert!(out.yaml.contains("_cloud_instance_region: eu-central-1"));
        assert!(!out.yaml.contains("us-east-1"));
    }

    #[test]
    fn preserves_comments_and_formatting() {
        let src = format!("# a note that matters\n{ENV}");
        let out = reskin(&src, &plan()).unwrap();
        assert!(
            out.yaml.contains("# a note that matters"),
            "comments must survive: an environment file carries authored \
             explanation the next person needs"
        );
    }

    #[test]
    fn rejects_an_environment_with_no_matching_prefix() {
        let mut p = plan();
        p.from_prefix = "nomatch-".into();
        let err = reskin(ENV, &p).unwrap_err();
        assert!(err.contains("nothing to re-skin"), "{err}");
    }

    #[test]
    fn rejects_a_file_with_no_guids() {
        let err = reskin("name: nothing\nnodes: []\n", &plan()).unwrap_err();
        assert!(err.contains("no node GUIDs"), "{err}");
    }

    #[test]
    fn guid_uniqueness_detects_a_clone() {
        let dir = std::env::temp_dir().join(format!("infra-sim-reskin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let existing = dir.join("existing.yaml");
        std::fs::write(&existing, ENV).unwrap();

        let mine = dir.join("new.yaml");
        let err = check_guid_uniqueness(&dir, ENV, &mine).unwrap_err();
        assert!(err.contains("already used by"), "{err}");
        assert!(
            err.contains("takes over the first node's identity"),
            "{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guid_uniqueness_ignores_the_file_being_written() {
        let dir = std::env::temp_dir().join(format!("infra-sim-reskin2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let self_path = dir.join("mine.yaml");
        std::fs::write(&self_path, ENV).unwrap();
        // Rewriting an environment in place is not a clone of itself.
        assert!(check_guid_uniqueness(&dir, ENV, &self_path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
