/// Stable textual refusal for the executor seam.
pub(super) fn executor_refusal(error: &crate::context_txn::executor::ExecutorError) -> String {
    use crate::context_txn::executor::ExecutorError;
    match error {
        ExecutorError::Audit(error) => format!("admission shadow audit failed: {error:?}"),
        ExecutorError::CapabilityNotLanded { op } => {
            format!("operation {op} has not landed in this phase")
        }
        ExecutorError::PreconditionFailed { which } => {
            format!("admission precondition {which} failed")
        }
        ExecutorError::StaleParent { expected, actual } => format!(
            "admission parent moved from {expected} to {actual}; refusing to commit stale state"
        ),
        ExecutorError::Fenced { held, mine } => {
            format!("admission fenced: held {held}, mine {mine}")
        }
        ExecutorError::AuthorityDenied { op, by } => {
            format!("admission of {op} denied for principal {by:?}")
        }
        ExecutorError::IllegalTransition { from, to } => {
            format!("admission transition {from:?} to {to:?} is illegal")
        }
        ExecutorError::NotRebaseSafe => "admission row is not rebase-safe".to_string(),
        ExecutorError::BoundDisagrees {
            claimed,
            computed,
        } => format!("admission bound {claimed} disagrees with the computed bound {computed}"),
        ExecutorError::Illegal { which, predicate } => {
            format!("admission legality gate rejected the projection: {which} ({predicate})")
        }
        ExecutorError::RegionOverBudget {
            admitted,
            projected,
            ceiling,
        } => format!(
            "admission region over budget: {admitted} admitted plus {projected} projected exceeds {ceiling}"
        ),
    }
}
