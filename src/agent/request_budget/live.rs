//! Production contract and calibration handoff to the existing send collector.
use super::calibration;
use crate::telemetry::bounds::{RequestBound, RequestContract};

pub(in crate::agent) fn collector(
    contract: Option<RequestContract>,
    context_limit: Option<u64>,
    observations: &[RequestBound],
) -> serdes_ai::models::telemetry::Collector {
    let Some(contract) = contract else {
        return serdes_ai::models::telemetry::Collector::default();
    };
    let observations = observations.to_vec();
    serdes_ai::models::telemetry::Collector::with_wire_gate(move |wire| {
        let bound =
            calibration::measure(contract.clone(), wire, None, context_limit, &observations)
                .map_err(|_| {
                    serdes_ai::models::ModelError::Configuration(
                        "request bound measurement failed".into(),
                    )
                })?;
        if let (Some(limit), Some(reserve)) = (context_limit, bound.terms.output_reserve) {
            let total = bound
                .bounded_input
                .and_then(|input| input.checked_add(reserve))
                .and_then(|n| n.checked_add(bound.terms.terminal_reserve));
            if bound.margin.is_some() && total.is_none_or(|total| total > limit) {
                return Err(serdes_ai::models::ModelError::RequestBound { limit });
            }
        }
        Ok(())
    })
}

impl super::super::Turn<'_> {
    pub(in crate::agent) fn restore_calibration(
        &self,
        store: &crate::session::SessionStore,
    ) -> Result<(), crate::session::StoreError> {
        *self.calibration.borrow_mut() = store.request_bound_observations()?;
        Ok(())
    }

    pub(in crate::agent) fn measure_bound(
        &self,
        wire: Option<&[u8]>,
        observed_input: Option<u64>,
    ) -> Result<Option<RequestBound>, calibration::CalibrationError> {
        let (Some(contract), Some(wire)) = (self.backend.request_contract(), wire) else {
            return Ok(None);
        };
        let mut bound = calibration::measure(
            contract,
            wire,
            None,
            self.context_limit,
            &self.calibration.borrow(),
        )?;
        bound.observed_input = observed_input;
        let mut observations = self.calibration.borrow_mut();
        if observations.len() == crate::telemetry::MAX_RECORDS {
            observations.remove(0);
        }
        observations.push(bound.clone());
        Ok(Some(bound))
    }
}
