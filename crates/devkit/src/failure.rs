use std::{fmt, io};

#[derive(Debug)]
/// An operation error with a machine-readable stage and code.
pub struct OperationFailure {
    /// Workflow stage where the operation failed.
    pub stage: &'static str,
    /// Stable error code for the failed stage.
    pub code: &'static str,
    source: io::Error,
}

impl fmt::Display for OperationFailure {
    // Preserve the underlying error text in human-readable output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for OperationFailure {
    // Expose the original I/O error to error-chain consumers.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Wraps an I/O error with workflow stage and error code metadata.
pub fn at(stage: &'static str, code: &'static str, source: io::Error) -> io::Error {
    io::Error::other(OperationFailure {
        stage,
        code,
        source,
    })
}

/// Converts a staged error into the CLI's JSON failure response.
pub fn report(error: &io::Error) -> serde_json::Value {
    let failure = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<OperationFailure>());
    let (stage, code) = failure
        .map(|e| (e.stage, e.code))
        .unwrap_or(("arguments", "invalid_arguments"));
    let mut report =
        serde_json::json!({"error": {"stage": stage, "code": code, "message": error.to_string()}});
    if matches!(stage, "transfer" | "register") {
        report["error"]["partial_update_possible"] = true.into();
        report["error"]["recovery"] = "Keep the game stopped, resolve the error, and rerun deploy before launching. No rollback is available.".into();
    }
    report
}
