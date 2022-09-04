use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Finalizer error: {0}")]
    FinalizerError(#[source] kube::runtime::finalizer::Error<kube::Error>),

    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error)
}

pub mod operator;
