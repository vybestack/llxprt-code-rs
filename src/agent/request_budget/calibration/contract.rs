//! Bind calibration to the selected serializer settings, tool declarations and head.
use super::{CalibrationError, RequestContract};
use sha2::{Digest, Sha256};

pub(super) fn selected(
    mut contract: RequestContract,
    wire: &[u8],
) -> Result<RequestContract, CalibrationError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(wire).map_err(|_| CalibrationError::Projection)?;
    if let Some(response) = value.get("response") {
        value = response.clone();
    }
    let mut head = vec![];
    for key in ["messages", "input"] {
        if let Some(messages) = value.get(key).and_then(|v| v.as_array()) {
            head.extend(
                messages
                    .iter()
                    .filter(|message| {
                        matches!(
                            message.get("role").and_then(|v| v.as_str()),
                            Some("system" | "developer")
                        )
                    })
                    .cloned(),
            );
        }
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }
    // Response identifiers select a continuation, not a new model/serializer contract.
    if let Some(object) = value.as_object_mut() {
        object.remove("previous_response_id");
    }
    let bytes = serde_json::to_vec(&(contract.identity.as_str(), value, head))
        .map_err(|_| CalibrationError::Projection)?;
    contract.identity = format!("{:x}", Sha256::digest(bytes));
    Ok(contract)
}
