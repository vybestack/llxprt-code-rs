//! Run observations do not participate in transcript-prefix or context materialization.
use super::*;

#[derive(Default)]
pub(super) struct Observations {
    pub operations: Vec<crate::telemetry::StoreOperation>,
    pub dropped: u64,
    pub repaired_tail: bool,
    pub reservation: String,
}

impl SessionStore {
    pub(crate) fn request_bound_observations(
        &self,
    ) -> Result<Vec<crate::telemetry::bounds::RequestBound>, StoreError> {
        self.locked(|| {
            let state = self.read()?;
            let mut observations: Vec<_> = state
                .branches
                .iter()
                .filter_map(|b| b.telemetry.as_ref())
                .flat_map(|t| &t.rounds)
                .filter_map(|c| c.request_bound.clone())
                .rev()
                .take(crate::telemetry::MAX_RECORDS)
                .collect();
            observations.reverse();
            Ok(observations)
        })
    }

    pub(super) fn begin_reservation_observation(&self) {
        let mut observations = self.observations.lock().expect("store observations mutex");
        if !observations.reservation.is_empty() {
            *observations = Observations::default();
        }
    }

    pub(crate) fn record_telemetry(
        &self,
        reserved: &ReservedRequest,
        telemetry: &crate::telemetry::Telemetry,
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let state = self.read()?;
            let branch = state
                .branches
                .iter()
                .find(|b| b.branch_id == reserved.branch_id)
                .ok_or(StoreError::Stale)?;
            if branch.owner != reserved.owner {
                return Err(StoreError::Stale);
            }
            self.append_event(log::Event::Telemetry {
                branch_id: reserved.branch_id.clone(),
                owner: reserved.owner.clone(),
                telemetry: Box::new(telemetry.clone()),
            })
        })
    }

    pub(crate) fn observe_store(&self, telemetry: &mut crate::telemetry::Telemetry) {
        let observations = self.observations.lock().expect("store observations mutex");
        telemetry.store_operations = observations.operations.clone();
        telemetry.dropped_records = telemetry
            .dropped_records
            .saturating_add(observations.dropped);
        telemetry.repaired_tail = observations.repaired_tail;
        telemetry.reservation = observations.reservation.clone();
    }

    pub(super) fn reservation_observed(&self, result: &Result<ReservedRequest, StoreError>) {
        let mut observations = self.observations.lock().expect("store observations mutex");
        // Reclaim explicitly sets its classification before returning.
        if observations.reservation == "stale-reclaim" {
            return;
        }
        observations.reservation = match result {
            Ok(r) if r.replay => "replay",
            Ok(r) if r.retry => "retry",
            Ok(_) => "fresh",
            Err(StoreError::Busy(_)) => "busy",
            Err(_) => "failed",
        }
        .into();
    }
}
