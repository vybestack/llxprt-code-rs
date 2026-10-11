use super::{StoreError, StreamRetryRecord};

pub(super) fn validate(record: &StreamRetryRecord) -> Result<(), StoreError> {
    if record.round == 0
        || !(1..=3).contains(&record.retry)
        || record.delay_ms != 250 * (1 << (record.retry - 1))
    {
        return Err(StoreError::Corrupt("invalid stream retry record".into()));
    }
    Ok(())
}
