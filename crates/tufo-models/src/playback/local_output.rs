//! Current local output health, independent of the lifetime error counters.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LocalOutputState {
    #[default]
    Released,
    Ready(LocalOutputIdentity),
    Recovering {
        previous_failure: Option<LocalOutputFailure>,
    },
    Failed(LocalOutputFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutputIdentity {
    pub backend: String,
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutputFailure {
    pub reason: LocalOutputFailureReason,
    pub identity: Option<LocalOutputIdentity>,
    /// Technical context for logs and CLI diagnostics, never GUI copy.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOutputFailureReason {
    NoDevice,
    TargetMissing,
    DeviceUnavailable,
    StreamInvalidated,
    DeviceChanged,
    CallbackStopped,
    OpenFailed,
    OpenTimedOut,
}
