//! Physical relationships shared by application telemetry producers.
use std::collections::BTreeMap;

/// Preserve modeled demand internally while publishing realizable occupancy.
pub fn normalize(values: &mut BTreeMap<String, f64>) {
    for (used, capacity) in [
        ("app_requests_error_rate", "app_requests_rate"),
        ("app_workers_busy", "app_workers_total"),
        ("app_db_pool_in_use", "app_db_pool_size"),
    ] {
        if let (Some(&used_value), Some(&capacity_value)) = (values.get(used), values.get(capacity))
        {
            if used_value.is_finite() && capacity_value.is_finite() {
                values.insert(used.into(), used_value.clamp(0.0, capacity_value.max(0.0)));
            }
        }
    }
    let mut floor = 0.0005;
    for key in ["app_latency_p50", "app_latency_p95", "app_latency_p99"] {
        if let Some(value) = values.get_mut(key) {
            if value.is_finite() {
                *value = value.max(floor);
                floor = *value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupancy_and_quantiles_obey_physical_relationships() {
        let mut values = BTreeMap::from([
            ("app_workers_busy".into(), 80.0),
            ("app_workers_total".into(), 32.0),
            ("app_db_pool_in_use".into(), 90.0),
            ("app_db_pool_size".into(), 40.0),
            ("app_latency_p50".into(), 0.2),
            ("app_latency_p95".into(), 0.1),
            ("app_latency_p99".into(), 0.15),
        ]);
        normalize(&mut values);
        assert_eq!(values["app_workers_busy"], 32.0);
        assert_eq!(values["app_db_pool_in_use"], 40.0);
        assert_eq!(values["app_latency_p95"], 0.2);
        assert_eq!(values["app_latency_p99"], 0.2);
    }
}
