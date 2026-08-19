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

/// The node GUIDs in an environment file.
///
/// Labels-block aware: a user label key `guid` (legal — nothing reserves it)
/// must not be mistaken for the node's identity line, or the invariance guard
/// would refuse a legitimate edit with a message about GUIDs.
fn guids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_labels = false;
    let mut labels_indent = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed == "labels:" {
            in_labels = true;
            labels_indent = indent;
            continue;
        }
        if in_labels && !trimmed.is_empty() && indent <= labels_indent {
            in_labels = false;
        }
        if in_labels {
            continue;
        }
        if let Some(g) = trimmed.strip_prefix("guid:") {
            out.push(g.trim().to_string());
        }
    }
    out
}

/// How one node's user-authored labels change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LabelChanges {
    /// Labels to set (inserted or updated in place).
    pub set: BTreeMap<String, String>,
    /// Labels to remove, by key.
    pub remove: std::collections::BTreeSet<String>,
}

impl LabelChanges {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }
}

/// What applying labels did.
#[derive(Debug)]
pub struct LabelOutcome {
    pub yaml: String,
    /// Hostnames whose label block actually changed.
    pub changed: Vec<String>,
}

/// The labels block of one node being walked.
struct LabelWalk {
    host: String,
    changes: LabelChanges,
    /// Indent of the block's entries (the `labels:` line's indent + 2).
    indent: usize,
    /// Index in the output of the `labels:` header line.
    header: usize,
    /// Keys seen in the block so far.
    seen: std::collections::BTreeSet<String>,
    /// Whether anything in this block was inserted, rewritten or dropped.
    dirty: bool,
}

/// Emit the keys the block is missing, and apply the tier the edits imply.
fn flush(
    walk: Option<LabelWalk>,
    out: &mut Vec<String>,
    tier_override: Option<String>,
    changed: &mut Vec<String>,
) {
    let Some(mut w) = walk else { return };
    for (key, value) in &w.changes.set {
        if !w.seen.contains(key) {
            out.push(format!(
                "{:i$}{key}: {}",
                "",
                crate::labels::yaml_scalar(value),
                i = w.indent
            ));
            w.dirty = true;
        }
    }
    if let Some(tier) = tier_override {
        // `environment` changed: the generated tier label follows it. Rewrite
        // the line in place when the block has one, else insert it right
        // under the header - a file without it is hand-written, and any
        // position inside the block is valid YAML.
        let rendered_tier = crate::labels::yaml_scalar(&tier);
        if let Some(pos) = out[w.header + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with("infra_sim_env:"))
            .map(|p| w.header + 1 + p)
        {
            let indent = out[pos].len() - out[pos].trim_start().len();
            out[pos] = format!("{:i$}infra_sim_env: {rendered_tier}", "", i = indent);
        } else {
            out.insert(
                w.header + 1,
                format!("{:i$}infra_sim_env: {rendered_tier}", "", i = w.indent),
            );
        }
        w.dirty = true;
    }
    if w.dirty && !changed.contains(&w.host) {
        changed.push(w.host.clone());
    }
}

/// Edit the user-authored labels of named nodes in an environment file,
/// leaving GUIDs and everything else untouched.
///
/// The same live-editing path re-skinning uses: rewrite the file, the plugin
/// notices the environment changed under it and exits cleanly, the agent
/// respawns it, and the agent migrates the vnode's labels in place — history,
/// trained ML and alert log all survive. `environment` is mirrored into the
/// generated `infra_sim_env` label so the two can never disagree after an
/// edit.
pub fn apply_labels(
    source: &str,
    per_host: &BTreeMap<String, LabelChanges>,
) -> Result<LabelOutcome, String> {
    // Validate before touching a line, so a refused edit cannot leave a
    // half-rewritten file behind.
    for (host, changes) in per_host {
        if changes.is_empty() {
            continue;
        }
        crate::labels::validate_map(&changes.set).map_err(|e| format!("{host}: {e}"))?;
        for key in &changes.remove {
            // The same rules as setting: this refuses generated keys too, so
            // `remove: infra_sim_role` cannot strip identity the runtime uses.
            crate::labels::validate_key(key)
                .map_err(|e| format!("{host}: cannot remove '{key}': {e}"))?;
        }
    }

    let before_guids = guids(source);
    if before_guids.is_empty() {
        return Err("no node GUIDs found; is this an environment file?".into());
    }

    let mut out: Vec<String> = Vec::new();
    let mut changed: Vec<String> = Vec::new();
    let mut touched: std::collections::BTreeSet<String> = Default::default();
    let mut current_host: Option<String> = None;
    let mut walk: Option<LabelWalk> = None;
    let mut tier_override: Option<String> = None;

    for line in source.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if trimmed.starts_with("- hostname:") {
            flush(walk.take(), &mut out, tier_override.take(), &mut changed);
            current_host = Some(
                trimmed
                    .strip_prefix("- hostname:")
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
            out.push(line.to_string());
            continue;
        }

        if let Some(host) = &current_host {
            let changes = per_host.get(host).filter(|c| !c.is_empty());

            if trimmed == "labels:" && walk.is_none() {
                let Some(changes) = changes.cloned() else {
                    out.push(line.to_string());
                    continue;
                };
                // Setting or removing `environment` retiers the node; removing
                // it falls back to the default tier rather than a stale one.
                tier_override = changes
                    .set
                    .get(crate::labels::ENVIRONMENT_LABEL)
                    .cloned()
                    .or_else(|| {
                        changes
                            .remove
                            .contains(crate::labels::ENVIRONMENT_LABEL)
                            .then(|| "production".to_string())
                    });
                touched.insert(host.clone());
                walk = Some(LabelWalk {
                    host: host.clone(),
                    changes,
                    indent: indent + 2,
                    header: out.len(),
                    seen: Default::default(),
                    dirty: false,
                });
                out.push(line.to_string());
                continue;
            }

            if let Some(w) = walk.as_mut() {
                // Blank and comment lines are transparent: a hand-edited file
                // may carry either inside a labels block, and closing the walk
                // on them would duplicate a later `set` key or strand a
                // later `remove`.
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    out.push(line.to_string());
                    continue;
                }
                if indent >= w.indent {
                    let key = trimmed.split(':').next().unwrap_or("").to_string();
                    if w.changes.remove.contains(&key) {
                        w.dirty = true;
                        continue; // the label line disappears entirely
                    }
                    if let Some(value) = w.changes.set.get(&key) {
                        out.push(format!(
                            "{:i$}{key}: {}",
                            "",
                            crate::labels::yaml_scalar(value),
                            i = w.indent
                        ));
                        w.dirty = true;
                    } else {
                        out.push(line.to_string());
                    }
                    w.seen.insert(key);
                    continue;
                }
                // Anything shallower ends the block.
                flush(walk.take(), &mut out, tier_override.take(), &mut changed);
                out.push(line.to_string());
                continue;
            }
        }

        out.push(line.to_string());
    }
    flush(walk.take(), &mut out, tier_override.take(), &mut changed);

    let missing: Vec<String> = per_host
        .iter()
        .filter(|(h, c)| !c.is_empty() && !touched.contains(*h))
        .map(|(h, _)| h.clone())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "no labels block found for {} - refusing to guess where labels belong",
            missing.join(", ")
        ));
    }

    let yaml = out.join("\n") + "\n";
    let after_guids = guids(&yaml);
    if before_guids != after_guids {
        return Err(
            "editing labels would change node GUIDs, which orphans history - refusing to write"
                .into(),
        );
    }

    // Defense in depth for the live-edit path: unlike create, there is no lint
    // between this write and a running simulation whose plugin will refuse to
    // respawn on a file it cannot parse. Validation should make this
    // unreachable; a hand-edited source file plus an edit must still never
    // take a fleet down while the console reports success.
    if serde_yaml::from_str::<serde_yaml::Value>(&yaml).is_err() {
        return Err(
            "the label edit would leave the environment unparseable - refusing to write; the \
             source file has a structure this editor does not understand"
                .into(),
        );
    }

    Ok(LabelOutcome { yaml, changed })
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

    fn changes(set: &[(&str, &str)], remove: &[&str]) -> BTreeMap<String, LabelChanges> {
        let mut c = LabelChanges::default();
        for (k, v) in set {
            c.set.insert(k.to_string(), v.to_string());
        }
        for k in remove {
            c.remove.insert(k.to_string());
        }
        BTreeMap::from([("sim-lb-01".to_string(), c)])
    }

    #[test]
    fn apply_inserts_missing_and_updates_existing_labels() {
        let src = ENV.replacen(
            "      infra_sim_env: production",
            "      infra_sim_env: production\n      team: old",
            1,
        );
        let out = apply_labels(
            &src,
            &changes(&[("team", "platform"), ("site", "dc-1")], &[]),
        )
        .unwrap();
        // Existing key updated in place, new key inserted in the block.
        assert!(out.yaml.contains("team: platform"), "{}", out.yaml);
        assert!(!out.yaml.contains("team: old"));
        assert!(out.yaml.contains("site: dc-1"));
        assert_eq!(out.changed, vec!["sim-lb-01".to_string()]);
    }

    #[test]
    fn apply_removes_label_lines() {
        let src = ENV.replacen(
            "      infra_sim_env: production",
            "      infra_sim_env: production\n      team: platform",
            1,
        );
        let out = apply_labels(&src, &changes(&[], &["team"])).unwrap();
        assert!(!out.yaml.contains("team: platform"), "{}", out.yaml);
        // The block still holds its generated labels.
        assert!(out.yaml.contains("infra_sim_env: production"));
    }

    #[test]
    fn apply_cannot_remove_generated_labels() {
        for key in [
            "infra_sim_env",
            "infra_sim_role",
            "latitude",
            "_os_name",
            "simulated",
        ] {
            let err = apply_labels(ENV, &changes(&[], &[key])).unwrap_err();
            assert!(err.contains("cannot remove"), "key {key}: {err}");
        }
    }

    #[test]
    fn setting_environment_retiers_the_node() {
        let out = apply_labels(ENV, &changes(&[("environment", "staging")], &[])).unwrap();
        assert!(out.yaml.contains("infra_sim_env: staging"), "{}", out.yaml);
        assert!(out.yaml.contains("environment: staging"));
        // The other node keeps its tier: edits are per host.
        assert!(out.yaml.matches("infra_sim_env: production").count() == 1);
    }

    #[test]
    fn removing_environment_resets_the_tier() {
        let src = ENV.replacen(
            "      infra_sim_env: production",
            "      infra_sim_env: staging\n      environment: staging",
            1,
        );
        let out = apply_labels(&src, &changes(&[], &["environment"])).unwrap();
        assert!(
            out.yaml.contains("infra_sim_env: production"),
            "{}",
            out.yaml
        );
        assert!(!out.yaml.contains("environment: staging"));
    }

    #[test]
    fn apply_preserves_guids_comments_and_siblings() {
        let src = format!("# a note that matters\n{ENV}");
        let out = apply_labels(&src, &changes(&[("team", "x")], &[])).unwrap();
        assert!(out.yaml.contains("# a note that matters"));
        assert!(out
            .yaml
            .contains("guid: 4a1f9d20-5e83-4c17-b6a2-0d94e7fc3518"));
        assert!(out.yaml.contains("_cloud_instance_region: us-east-1"));
    }

    #[test]
    fn blank_and_comment_lines_inside_a_labels_block_are_survivable() {
        // Hand-edited files carry these; closing the walk on them duplicated
        // a later `set` key or stranded a later `remove`.
        let src = ENV.replacen(
            "      infra_sim_env: production",
            "      infra_sim_env: production\n      team: old\n\n      # a note\n      site: dc-1",
            1,
        );
        let out = apply_labels(&src, &changes(&[("team", "new")], &["site"])).unwrap();
        assert!(out.yaml.contains("team: new"), "{}", out.yaml);
        assert!(!out.yaml.contains("team: old"));
        assert!(
            !out.yaml.contains("site: dc-1"),
            "remove must reach past the blank"
        );
        assert_eq!(
            out.yaml.matches("site:").count(),
            0,
            "the only site label was the removed one"
        );
        assert!(out.yaml.contains("# a note"), "comments survive");
        // And it is still valid YAML with exactly one value per key.
        #[derive(serde::Deserialize)]
        struct EnvLike {
            nodes: Vec<NodeLike>,
        }
        #[derive(serde::Deserialize)]
        struct NodeLike {
            labels: BTreeMap<String, String>,
        }
        let parsed: EnvLike = serde_yaml::from_str(&out.yaml).unwrap();
        assert_eq!(
            parsed.nodes[0].labels.get("team").map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn a_guid_label_key_does_not_trip_the_guid_guard() {
        // `guid` is a legal label key; treating its line as node identity made
        // adding it fail with a bogus message about orphaning history.
        let mut c = LabelChanges::default();
        c.set.insert("guid".into(), "my-service".into());
        let out = apply_labels(ENV, &BTreeMap::from([("sim-lb-01".to_string(), c)])).unwrap();
        assert!(out.yaml.contains("guid: my-service"), "{}", out.yaml);
        // The node's real GUID line is untouched.
        assert!(out
            .yaml
            .contains("guid: 4a1f9d20-5e83-4c17-b6a2-0d94e7fc3518"));
    }

    #[test]
    fn apply_refuses_an_unknown_hostname_rather_than_no_op() {
        let mut c = LabelChanges::default();
        c.set.insert("team".into(), "x".into());
        let err =
            apply_labels(ENV, &BTreeMap::from([("sim-ghost-01".to_string(), c)])).unwrap_err();
        assert!(err.contains("sim-ghost-01"), "{err}");
    }
}
