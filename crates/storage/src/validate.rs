// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! Input-accuracy tier for typed observations.
//!
//! Every observation is checked against its registered [`AttributeDef`] before
//! it is allowed to influence derivations. Rows that fail — wrong type, out of
//! plausibility range, or referencing an unregistered attribute — are marked
//! `Quarantined` (stored for audit, excluded from accumulation) rather than
//! silently dropped. This is the gate that keeps "ingest any data" from
//! becoming "derive confidently-wrong predictions from garbage".
//!
//! Pure functions only: the caller fetches the registry and feeds it in.

use std::collections::HashMap;

use memory_core::{AttributeDef, ObservationStatus, TypedObservation};

/// Registry key: an attribute is identified by `(projectId, attributeKey)`
/// within a platform. `projectId` is normalised to `""` when absent so a
/// platform-wide definition has a stable key.
pub type RegistryKey = (String, String);

/// Build the lookup the validator needs from a flat list of definitions.
pub fn index_defs(defs: &[AttributeDef]) -> HashMap<RegistryKey, AttributeDef> {
    defs.iter()
        .map(|d| {
            (
                (
                    d.project_id.clone().unwrap_or_default(),
                    d.attribute_key.clone(),
                ),
                d.clone(),
            )
        })
        .collect()
}

/// Outcome of validating one observation.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: ObservationStatus,
    pub quality_score: f32,
    /// Set only when quarantined — why the row was rejected.
    pub reason: Option<String>,
}

/// Validate one observation against the registry. Resolution order: an exact
/// `(projectId, key)` definition wins; a platform-wide (`projectId = None`)
/// definition is the fallback; no definition at all → quarantine.
pub fn validate_one(obs: &TypedObservation, defs: &HashMap<RegistryKey, AttributeDef>) -> Outcome {
    let project_key = obs.project_id.clone().unwrap_or_default();
    let def = defs
        .get(&(project_key, obs.attribute_key.clone()))
        .or_else(|| defs.get(&(String::new(), obs.attribute_key.clone())));

    let Some(def) = def else {
        return Outcome {
            status: ObservationStatus::Quarantined,
            quality_score: 0.0,
            reason: Some(format!(
                "no registered attribute definition for '{}'",
                obs.attribute_key
            )),
        };
    };

    match def.validate(obs) {
        Ok(()) => Outcome {
            // Trust-weighted quality: a valid row from a low-trust source is
            // still worth less than the same row from a trusted one.
            status: ObservationStatus::Accepted,
            quality_score: obs.trust.clamp(0.0, 1.0),
            reason: None,
        },
        Err(reason) => Outcome {
            status: ObservationStatus::Quarantined,
            quality_score: 0.0,
            reason: Some(reason),
        },
    }
}

/// Validate a batch in place, returning a copy of each observation with
/// `status` + `quality_score` set, paired with an optional quarantine reason.
pub fn validate_batch(
    observations: &[TypedObservation],
    defs: &HashMap<RegistryKey, AttributeDef>,
) -> Vec<(TypedObservation, Option<String>)> {
    observations
        .iter()
        .map(|obs| {
            let outcome = validate_one(obs, defs);
            let mut validated = obs.clone();
            validated.status = outcome.status;
            validated.quality_score = outcome.quality_score;
            (validated, outcome.reason)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use memory_core::DataType;

    fn credit_def() -> AttributeDef {
        AttributeDef {
            id: "def1".into(),
            platform_id: "p".into(),
            project_id: Some("proj".into()),
            attribute_key: "credit_score".into(),
            data_type: DataType::Numeric,
            unit: None,
            min_valid: Some(300.0),
            max_valid: Some(850.0),
            required: false,
            metadata: serde_json::Value::Null,
        }
    }

    fn obs(value: f64) -> TypedObservation {
        let mut o = TypedObservation::numeric(
            "o1",
            "p",
            "contact",
            "c1",
            "credit_score",
            value,
            Utc::now(),
        );
        o.project_id = Some("proj".into());
        o
    }

    #[test]
    fn accepts_in_range() {
        let defs = index_defs(&[credit_def()]);
        let out = validate_one(&obs(650.0), &defs);
        assert_eq!(out.status, ObservationStatus::Accepted);
        assert!(out.reason.is_none());
    }

    #[test]
    fn quarantines_out_of_range() {
        let defs = index_defs(&[credit_def()]);
        let out = validate_one(&obs(9000.0), &defs);
        assert_eq!(out.status, ObservationStatus::Quarantined);
        assert!(out.reason.unwrap().contains("above maxValid"));
    }

    #[test]
    fn quarantines_unregistered() {
        let defs = index_defs(&[]);
        let out = validate_one(&obs(650.0), &defs);
        assert_eq!(out.status, ObservationStatus::Quarantined);
        assert!(out.reason.unwrap().contains("no registered"));
    }

    #[test]
    fn quality_tracks_trust() {
        let defs = index_defs(&[credit_def()]);
        let mut o = obs(700.0);
        o.trust = 0.5;
        let out = validate_one(&o, &defs);
        assert_eq!(out.status, ObservationStatus::Accepted);
        assert!((out.quality_score - 0.5).abs() < 1e-6);
    }
}
