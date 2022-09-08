use kube::runtime::wait::Error;
pub use operator::operator::*;
use actix_web::{HttpRequest, Responder, HttpResponse, get, HttpServer, App, web::Data, middleware};
use tracing::{info, warn, subscriber, Level};
use tracing_subscriber::{prelude::*, EnvFilter, Registry, fmt, FmtSubscriber};

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
    //let logger = tracing_subscriber::fmt::layer();
    // let env_filter = EnvFilter::try_from_default_env()
    //     .or_else(|_| EnvFilter::try_new("info"))
    //     .unwrap();
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Setting default subscriber failed");

    // Decide on layers
    // TODO
    

    // Initialize tracing
    // tracing::subscriber::set_global_default(collector).unwrap(),

    // Start kubernetes controller
    let (operator, controller) = Operator::new().await;

    // Start web server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(operator.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
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
