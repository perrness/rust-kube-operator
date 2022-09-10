use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Finalizer error: {0}")]
    FinalizerError(#[source] kube::runtime::finalizer::Error<kube::Error>),

    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error)
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// State machinery for kubernetes, exposable to actix
pub mod operator;
pub use operator::Operator;

/// Generate type, for crdgen
pub use operator::Application;

/// Deployments
pub mod deployment;

/// Log and trace integrations
pub mod telemetry;
