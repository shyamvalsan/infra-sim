//! The go.d scrape configuration for the simulated Prometheus exporters.
//!
//! One generator, two consumers (the console's local install and the
//! containerised path via `infra-sim.plugin --exporter-config`), so the two
//! can never drift into different chart identities.
//!
//! ## Why job names carry no hostname
//!
//! go.d prefixes every scraped chart's ID with the job name, and Netdata's
//! grouped views key off the chart *context*, whose app segment comes from the
//! job's `app` when set (netdata/netdata @ c23face0bd94
//! `src/go/plugin/go.d/collector/prometheus/runtime.go:115-139`,
//! `config_schema.json:44-48`). Two consequences, both verified against a
//! live agent (SOW-0020 probe):
//!
//! * a **shared `app`** is what makes every node's scraped metrics share one
//!   context (`prometheus.infra_sim_app.app_orders_total`) - the difference
//!   between an aggregatable application tier and N per-node islands;
//! * the job **name** is part of the chart ID, so a name derived from the
//!   hostname would churn every chart ID on each re-skin and orphan the
//!   scraped history. Job names are therefore `infra_sim_app_{role}_{nn}`:
//!   roles do not change when a fleet is re-skinned, and the per-role index
//!   follows environment order, so the same environment always produces the
//!   same names.

use std::fmt::Write as _;

/// The app segment of every scraped chart's context. A fixed literal (not the
/// simulation's name) on purpose: contexts are identity, and identity that
/// changed on a re-skin would orphan every scraped chart's history the same
/// way a changed GUID orphans a node.
pub const APP: &str = "infra_sim_app";

/// The port the exporter server listens on inside its own network namespace.
pub const DEFAULT_PORT: u16 = 19998;

/// One node the scrape config covers.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRef {
    pub hostname: String,
    pub guid: String,
    pub role: String,
}

/// The job name for a node's scrape: `infra_sim_app_{role_slug}_{nn}`.
///
/// Role dashes become underscores (a job name is a chart-ID prefix, and an
/// ID is dotted, so a dash would move house); the index is per role, in
/// environment order, zero-padded to keep names sorting correctly.
pub fn job_name(role: &str, index: usize) -> String {
    let slug = role.replace('-', "_");
    format!("{APP}_{slug}_{index:02}")
}

/// The vnode registry go.d needs before any job may attribute to a vnode.
///
/// go.d reads this file once, at startup (netdata/netdata @ c23face0bd94
/// `src/go/plugin/agent/setup.go:179`), which is why whoever writes it must
/// also restart go.d once - the agent respawns external plugins within
/// seconds, and no netdatacli command exists for this.
pub fn vnodes_conf(nodes: &[NodeRef]) -> String {
    let mut out = String::new();
    for n in nodes {
        let _ = writeln!(out, "- hostname: {}", n.hostname);
        let _ = writeln!(out, "  guid: {}", n.guid);
    }
    out
}

/// The go.d prometheus jobs: one per node, shared app, vnode-attributed.
pub fn go_d_conf(nodes: &[NodeRef], port: u16) -> String {
    let mut counters: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut out = String::from("jobs:\n");
    for n in nodes {
        let index = counters.entry(n.role.as_str()).or_insert(0);
        *index += 1;
        let name = job_name(&n.role, *index);
        let _ = writeln!(out, "  - name: {name}");
        let _ = writeln!(out, "    app: {APP}");
        let _ = writeln!(out, "    vnode: {}", n.hostname);
        let _ = writeln!(
            out,
            "    url: http://127.0.0.1:{port}/metrics/{}",
            n.hostname
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<NodeRef> {
        vec![
            NodeRef {
                hostname: "acme-lb-01".into(),
                guid: "aaaa".into(),
                role: "lb".into(),
            },
            NodeRef {
                hostname: "acme-web-01".into(),
                guid: "bbbb".into(),
                role: "web".into(),
            },
            NodeRef {
                hostname: "acme-web-02".into(),
                guid: "cccc".into(),
                role: "web".into(),
            },
            NodeRef {
                hostname: "acme-sw-01".into(),
                guid: "dddd".into(),
                role: "network-device".into(),
            },
        ]
    }

    #[test]
    fn job_names_carry_no_hostname_and_index_per_role() {
        let conf = go_d_conf(&nodes(), DEFAULT_PORT);
        // Per-role indices, not global: the second web node is _02 even
        // though it is the third node overall.
        assert!(conf.contains("name: infra_sim_app_lb_01\n"));
        assert!(conf.contains("name: infra_sim_app_web_01\n"));
        assert!(conf.contains("name: infra_sim_app_web_02\n"));
        // A dash in a role cannot leak into a chart-ID-shaped name.
        assert!(conf.contains("name: infra_sim_app_network_device_01\n"));
        // The hostnames a re-skin rewrites appear only where they must:
        // attribution and the scrape URL, never the job name.
        assert!(!conf.contains("name: infra_sim_app_acme"));
    }

    #[test]
    fn every_job_shares_the_app_and_points_at_its_vnode() {
        let conf = go_d_conf(&nodes(), DEFAULT_PORT);
        assert_eq!(conf.matches("app: infra_sim_app").count(), 4);
        assert!(conf.contains("vnode: acme-web-02\n"));
        assert!(conf.contains("url: http://127.0.0.1:19998/metrics/acme-web-02"));
    }

    #[test]
    fn a_re_skin_produces_identical_job_names() {
        // Same shape, renamed hosts: the chart IDs cannot change, because
        // they are prefixed by these names.
        let before = go_d_conf(&nodes(), DEFAULT_PORT);
        let renamed: Vec<NodeRef> = nodes()
            .into_iter()
            .map(|n| NodeRef {
                hostname: n.hostname.replace("acme", "initech"),
                ..n
            })
            .collect();
        let after = go_d_conf(&renamed, DEFAULT_PORT);
        let names = |c: &str| -> Vec<String> {
            c.lines()
                .filter_map(|l| l.strip_prefix("  - name: ").map(str::to_string))
                .collect()
        };
        assert_eq!(names(&before), names(&after));
        assert!(after.contains("vnode: initech-web-01"));
    }

    #[test]
    fn the_same_input_always_produces_the_same_output() {
        assert_eq!(
            go_d_conf(&nodes(), DEFAULT_PORT),
            go_d_conf(&nodes(), DEFAULT_PORT)
        );
        assert_eq!(vnodes_conf(&nodes()), vnodes_conf(&nodes()));
    }

    #[test]
    fn the_vnode_registry_pairs_every_hostname_with_its_guid() {
        let conf = vnodes_conf(&nodes());
        assert!(conf.contains("- hostname: acme-web-01\n  guid: bbbb\n"));
        assert_eq!(conf.matches("- hostname:").count(), 4);
    }
}
