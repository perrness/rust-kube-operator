use opentelemetry::trace::TraceId;

/// Fetch opeletelemetry::trace::TraceId as hex through entire stack
pub fn get_trace_id() -> TraceId {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
}

