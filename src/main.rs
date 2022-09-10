use kube::runtime::wait::Error;
pub use operator::operator::*;
use actix_web::{HttpRequest, Responder, HttpResponse, get, HttpServer, App, web::Data, middleware};
use prometheus::{TextEncoder, Encoder};
use tracing::{info, warn};
use tracing_subscriber::{prelude::*, EnvFilter, Registry};

#[get("/metrics")]
async fn metrics(c: Data<Operator>, _req: HttpRequest) -> impl Responder {
    let metrics = c.metrics();
    let encoder = TextEncoder::new();
    let mut buffer = vec![];
    encoder.encode(&metrics, &mut buffer).unwrap();
    HttpResponse::Ok().body(buffer)
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<Operator>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Setup tracing layers
    #[cfg(feature = "telemtry")]
    let telemetry = tracing_opentelemetry::layer().with_tracer(telemetry::init_tracer().await);
    let logger = tracing_subscriber::fmt::layer();
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    // let subscriber = FmtSubscriber::builder()
    //     .with_max_level(Level::INFO)
    //     .finish();
    // tracing::subscriber::set_global_default(subscriber)
    //     .expect("Setting default subscriber failed");

    // Decide on layers
    #[cfg(feature = "telemetry")]
    let Collector =Registry::default().with(telemetry).with(logger).with(env_filter);
    #[cfg(not(feature = "telemetry"))]
    let collector = Registry::default().with(logger).with(env_filter);

    // Initialize tracing
    tracing::subscriber::set_global_default(collector).unwrap();

    // Start kubernetes controller
    let (operator, controller) = Operator::new().await;

    // Start web server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(operator.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
            .service(metrics)
    })
    .bind("0.0.0.0:8080")
    .expect("Can not bind to 0.0.0.0:8080")
    .shutdown_timeout(5);

    tokio::select! {
        _ = controller => warn!("controller exited"),
        _ = server.run() => info!("actix exited"),
    }

    Ok(())
}
