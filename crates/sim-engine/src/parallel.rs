//! Per-node work spread across cores.
//!
//! Every `NodeEngine` owns its own signal state and RNG stream, and the spec
//! behind it is shared read-only, so nodes can be advanced concurrently without
//! affecting each other's values. That property is what makes the fidelity lint
//! parallelisable at all: a 25-node fleet linted sequentially cost 115s of a
//! single core, which is most of the time an operator waits for `create`.
//!
//! Results come back in node order. A lint whose output depends on thread
//! scheduling cannot be diffed against the previous run, and diffing it is how
//! we prove parallelism changed nothing.

use std::thread;

use crate::NodeEngine;

/// Apply `f` to every engine concurrently, returning results in node order.
///
/// Falls back to a plain sequential pass when there is one core or one node, so
/// a container with a single CPU behaves exactly as before.
pub fn map_engines<T, F>(engines: &mut [NodeEngine], f: F) -> Vec<T>
where
    F: Fn(&mut NodeEngine) -> T + Send + Sync,
    T: Send,
{
    let nodes = engines.len();
    let workers = thread::available_parallelism()
        .map_or(1, |p| p.get())
        .min(nodes);
    if workers <= 1 {
        return engines.iter_mut().map(f).collect();
    }

    // A static chunk split rather than work stealing: per-node cost is uniform
    // (identical tick counts over specs of similar size), so there is nothing to
    // balance, and no coordination means nothing that could reorder results.
    let chunk = nodes.div_ceil(workers);
    let f = &f;
    thread::scope(|scope| {
        let handles: Vec<_> = engines
            .chunks_mut(chunk)
            .map(|group| scope.spawn(move || group.iter_mut().map(f).collect::<Vec<T>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("lint worker panicked"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeProfile, ScenarioSet};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const SPEC: &str = r#"
version: 1
name: parallel-test
signals:
  load:
    base: 40.0
    min: 0.0
    max: 100.0
    noise: { kind: walk, sigma: 0.05 }
contexts:
  - id: system.load
    title: Load
    units: percentage
    family: load
    chart_type: line
    priority: 100
    shape: independent
    dimensions:
      - { id: value, signal: load }
"#;

    fn engines(count: usize) -> Vec<NodeEngine> {
        let spec: Arc<sim_spec::GeneratorSpec> =
            Arc::new(serde_yaml::from_str(SPEC).expect("test spec parses"));
        (0..count)
            .map(|i| {
                let profile = NodeProfile {
                    guid: format!("guid-{i:02}"),
                    hostname: format!("node-{i:02}"),
                    role: None,
                    attrs: BTreeMap::new(),
                    labels: BTreeMap::new(),
                    instances: BTreeMap::new(),
                    utc_offset_secs: 0,
                };
                NodeEngine::new(Arc::clone(&spec), profile, 42)
            })
            .collect()
    }

    #[test]
    fn results_come_back_in_node_order() {
        let mut e = engines(9);
        let names = map_engines(&mut e, |eng| eng.profile().hostname.clone());
        let expected: Vec<String> = (0..9).map(|i| format!("node-{i:02}")).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn parallel_ticking_matches_sequential() {
        let ticks = 200;
        let mut seq = engines(9);
        for eng in seq.iter_mut() {
            for i in 0..ticks {
                eng.tick(&ScenarioSet::default(), 1_700_000_000 + i, 1.0);
            }
        }
        let mut par = engines(9);
        map_engines(&mut par, |eng| {
            for i in 0..ticks {
                eng.tick(&ScenarioSet::default(), 1_700_000_000 + i, 1.0);
            }
        });

        // Compare the next sample from each: identical values prove the two runs
        // left every signal's state and RNG stream in the same place.
        let flatten = |s: &[crate::Sample]| -> Vec<(String, Vec<(String, i64)>)> {
            s.iter()
                .map(|x| (x.chart_id.clone(), x.values.clone()))
                .collect()
        };
        for (a, b) in seq.iter_mut().zip(par.iter_mut()) {
            let host = a.profile().hostname.clone();
            let sa = flatten(&a.tick(&ScenarioSet::default(), 1_700_000_300, 1.0));
            let sb = flatten(&b.tick(&ScenarioSet::default(), 1_700_000_300, 1.0));
            assert_eq!(sa, sb, "node {host} diverged");
        }
    }
}
