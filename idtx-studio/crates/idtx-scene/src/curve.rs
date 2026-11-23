//! Snapshot-owned bounded curve tables. Entity generation guards handles;
//! value comparison avoids rebuilding on unrelated stage revisions.
use bevy_ecs::prelude::*;
pub use idtx_usd::curve::{BasisCurvesRecord, CurveError, CurveEvaluator, CurveSample, CurveSpace};
use std::sync::Arc;

#[derive(Component, Debug, Clone)]
pub struct CurveComp {
    pub source: BasisCurvesRecord,
    pub time_code: Option<f64>,
    pub geometric_revision: u64,
    pub tables: Vec<Result<Arc<CurveEvaluator>, CurveError>>,
    validation: Result<(), CurveError>,
}

impl CurveComp {
    pub fn new(source: BasisCurvesRecord, time_code: Option<f64>, revision: u64) -> Self {
        let validation = source.validate();
        let mut samples = 0;
        let tables = (0..source.counts.len().min(idtx_usd::curve::MAX_CURVE_POINTS))
            .map(|i| {
                if samples >= idtx_usd::curve::MAX_CURVE_SAMPLES {
                    return Err(CurveError::Limit);
                }
                let table = CurveEvaluator::new(&source, i)?;
                samples += table.table_samples();
                if samples > idtx_usd::curve::MAX_CURVE_SAMPLES {
                    return Err(CurveError::Limit);
                }
                Ok(Arc::new(table))
            })
            .collect();
        Self {
            source,
            time_code,
            geometric_revision: revision,
            tables,
            validation,
        }
    }

    pub fn curve(&self, index: usize) -> Result<&CurveEvaluator, CurveError> {
        self.validation.clone()?;
        self.tables.get(index).ok_or(CurveError::Index)?.as_deref().map_err(Clone::clone)
    }
}
