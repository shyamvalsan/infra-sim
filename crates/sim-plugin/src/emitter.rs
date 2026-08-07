//! plugins.d protocol emitter.
//!
//! Protocol reference: `netdata/netdata` `src/plugins.d/README.md`. The
//! sequence that matters is `HOST_DEFINE` / `HOST_LABEL` / `HOST_DEFINE_END`
//! to create virtual nodes, then `HOST <guid>` to switch collection context
//! before each node's charts and samples.
//!
//! Virtual-node creation is available to external plugins, not just go.d:
//! `HOST_DEFINE` is registered with `PARSER_INIT_PLUGINSD` and lands in
//! `rrdhost_find_or_create(..., NETDATA_VIRTUAL_HOST, ...)`. This was verified
//! against a live agent before the architecture was committed; see
//! `prototypes/vnode-probe/FINDINGS.md`.

use std::io::{self, Write};

use sim_engine::{NodeEngine, NodeProfile, Sample};
use sim_spec::{GeneratorSpec, Shape};

/// Plugin name reported on every chart.
pub const PLUGIN_NAME: &str = "infra-sim.plugin";
/// Module name reported on every chart.
pub const MODULE_NAME: &str = "sim";

/// Quote a value for the plugins.d word splitter.
///
/// The parser splits lines on whitespace, so an unquoted `Ubuntu 24.04 LTS`
/// silently truncates to `Ubuntu`. That was the first bug the verification
/// probe hit, and it fails silently in a way that only shows up as a subtly
/// wrong node-info panel.
fn quote(value: &str) -> String {
    // Embedded single quotes would terminate the quoted run; no label value we
    // generate needs them, so drop them rather than invent an escape the
    // parser does not implement.
    let cleaned: String = value
        .chars()
        .filter(|c| *c != '\'' && !c.is_control())
        .collect();
    format!("'{cleaned}'")
}

/// Declare every simulated node as a Netdata virtual node.
pub fn define_hosts<W: Write>(out: &mut W, profiles: &[NodeProfile]) -> io::Result<()> {
    for p in profiles {
        writeln!(out, "HOST_DEFINE {} {}", p.guid, p.hostname)?;
        for (key, value) in &p.labels {
            writeln!(out, "HOST_LABEL {} {}", key, quote(value))?;
        }
        writeln!(out, "HOST_DEFINE_END")?;
    }
    Ok(())
}

/// Declare one node's charts and dimensions from its planned chart list.
///
/// A vnode carries exactly the contexts its plugin declares — the agent adds no
/// node-level dashboard sections on its own — so this call determines how
/// complete a simulated node's dashboard looks.
///
/// The plan comes from the engine rather than being re-derived from the spec.
/// Deriving it twice would let declaration and emission drift apart, sending
/// `SET` lines to charts that were never declared — which surfaces as silently
/// missing data, not an error.
pub fn declare_charts<W: Write>(
    out: &mut W,
    spec: &GeneratorSpec,
    engine: &NodeEngine,
    update_every: i64,
) -> io::Result<()> {
    writeln!(out, "HOST {}", engine.profile().guid)?;
    for chart in engine.charts() {
        let ctx = &spec.contexts[chart.context_index];
        writeln!(
            out,
            "CHART {} '' {} {} {} {} {} {} {} '' {} {}",
            chart.chart_id,
            quote(&ctx.title),
            quote(&ctx.units),
            quote(&chart.family),
            quote(&chart.context_id),
            ctx.chart_type.as_str(),
            ctx.priority,
            update_every,
            quote(PLUGIN_NAME),
            quote(MODULE_NAME),
        )?;
        write_dimensions(out, &ctx.shape)?;
    }
    Ok(())
}

fn write_dimensions<W: Write>(out: &mut W, shape: &Shape) -> io::Result<()> {
    // Each arm repeats the same DIMENSION line because the three dimension
    // types are distinct structs with no shared trait; a trait here would buy
    // nothing but indirection.
    match shape {
        Shape::Independent { dimensions } => {
            for d in dimensions {
                let name = d.name.as_deref().unwrap_or(&d.id);
                writeln!(
                    out,
                    "DIMENSION {} {} {} {} {} ''",
                    d.id,
                    quote(name),
                    d.algorithm.as_str(),
                    d.multiplier,
                    d.divisor
                )?;
            }
        }
        Shape::Partition { dimensions, .. } => {
            for d in dimensions {
                let name = d.name.as_deref().unwrap_or(&d.id);
                writeln!(
                    out,
                    "DIMENSION {} {} {} {} {} ''",
                    d.id,
                    quote(name),
                    d.algorithm.as_str(),
                    d.multiplier,
                    d.divisor
                )?;
            }
        }
        Shape::Counters { dimensions } => {
            for d in dimensions {
                let name = d.name.as_deref().unwrap_or(&d.id);
                writeln!(
                    out,
                    "DIMENSION {} {} {} {} {} ''",
                    d.id,
                    quote(name),
                    d.algorithm.as_str(),
                    d.multiplier,
                    d.divisor
                )?;
            }
        }
    }
    Ok(())
}

/// Emit one node's samples for a tick.
pub fn emit_samples<W: Write>(out: &mut W, guid: &str, samples: &[Sample]) -> io::Result<()> {
    writeln!(out, "HOST {guid}")?;
    for sample in samples {
        writeln!(out, "BEGIN {}", sample.chart_id)?;
        for (dim, value) in &sample.values {
            writeln!(out, "SET {dim} = {value}")?;
        }
        writeln!(out, "END")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn profile() -> NodeProfile {
        NodeProfile {
            guid: "9c6b1e42-7a3f-4d18-9e57-2b40c8f1a601".into(),
            hostname: "sim-web-01".into(),
            role: Some("web".into()),
            attrs: BTreeMap::new(),
            labels: BTreeMap::from([
                ("_os_name".to_string(), "Ubuntu".to_string()),
                ("_os_version".to_string(), "24.04.1 LTS".to_string()),
                ("simulated".to_string(), "true".to_string()),
            ]),
            instances: BTreeMap::new(),
            utc_offset_secs: 0,
        }
    }

    fn render<F>(f: F) -> String
    where
        F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
    {
        let mut buf = Vec::new();
        f(&mut buf).expect("write should succeed");
        String::from_utf8(buf).expect("output is utf-8")
    }

    #[test]
    fn host_definition_follows_the_protocol_sequence() {
        let out = render(|w| define_hosts(w, &[profile()]));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "HOST_DEFINE 9c6b1e42-7a3f-4d18-9e57-2b40c8f1a601 sim-web-01"
        );
        assert_eq!(lines.last().copied(), Some("HOST_DEFINE_END"));
    }

    #[test]
    fn label_values_with_spaces_are_quoted() {
        // Regression guard for the probe's first bug: unquoted "24.04.1 LTS"
        // truncates to "24.04.1" because the parser splits on whitespace.
        let out = render(|w| define_hosts(w, &[profile()]));
        assert!(
            out.contains("HOST_LABEL _os_version '24.04.1 LTS'"),
            "label not quoted:\n{out}"
        );
    }

    #[test]
    fn quoting_strips_characters_the_parser_cannot_escape() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("has space"), "'has space'");
        assert_eq!(quote("it's"), "'its'");
        assert_eq!(quote("line\nbreak"), "'linebreak'");
    }

    #[test]
    fn samples_emit_begin_set_end_per_context() {
        let samples = vec![Sample {
            chart_id: "system.cpu".into(),
            values: vec![("user".into(), 42), ("idle".into(), 58)],
        }];
        let out = render(|w| emit_samples(w, "guid-1", &samples));
        assert_eq!(
            out,
            "HOST guid-1\nBEGIN system.cpu\nSET user = 42\nSET idle = 58\nEND\n"
        );
    }
}
