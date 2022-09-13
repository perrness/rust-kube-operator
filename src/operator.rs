use std::{sync::Arc, time::Duration};

use chrono::DateTime;
use futures::{future::BoxFuture, FutureExt, StreamExt};
use k8s_openapi::{chrono::Utc, api::apps::v1::Deployment};
use kube::{
    CustomResource, Client, 
    runtime::{
        events::{Recorder, Reporter, EventType, Event},
        controller::Action, finalizer, Controller, 
    }, 
    ResourceExt, Api, Resource, api::{Patch, PatchParams, ListParams}
};
use prometheus::{IntCounter, HistogramVec, register_histogram_vec, register_int_counter, proto::MetricFamily, default_registry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{sync::RwLock, time::Instant};
use tracing::{instrument, info, warn, Span, field};

use crate::{Error, telemetry, deployment::{create_deployment, cleanup_deployment}};

static CUSTOM_APP_FINALIZER: &str = "customapps.per.naess";

#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
enum ApplicationState {
    Running,
    Starting,
    Failed,
}
/// Generate the Kubernetes wrapper struct "Application" from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml(in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(kind = "Application", group = "per.naess", version = "v1alpha1", namespaced)]
#[kube(status = "ApplicationStatus", shortname = "app")]
pub struct ApplicationSpec {
    pub name: String,
    pub image: String,
    pub deploy: bool,
}

/// The status object of  `Application`
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ApplicationStatus {
    state: ApplicationState,
    deployed: bool,
}

impl Application {
    // fn was_hidden(&self) -> bool {
    //     self.status.as_ref().map(|s| s.hidden).unwrap_or(false)
    // }
    fn was_deployed(&self) -> bool {
        self.status.as_ref().map(|s| s.deployed).unwrap_or(false)
    }

    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, kube::Error> {
        let client = ctx.client.clone();
        ctx.diagnostics.write().await.last_event = Utc::now();
        let reporter = ctx.diagnostics.read().await.reporter.clone();
        let recorder = Recorder::new(client.clone(), reporter, self.object_ref(&()));
        let name = self.name_any();
        let ns = self.namespace().unwrap();
        let apps: Api<Application> = Api::namespaced(client.clone(), &ns);

        let application_state: ApplicationState = ApplicationState::Running;

        // Handle deployment
        let should_deploy = self.spec.deploy;
        handle_deployment(&self, &ns, client, recorder, &name).await?;

        // let should_hide = self.spec.hide;
        // if self.was_hidden() && should_hide {
        //     // only send event first time
        //     recorder
        //         .publish(Event {
        //             type_: EventType::Normal,
        //             reason: "HiddenApplication".into(),
        //             note: Some(format!("Hiding `{}`", name)),
        //             action: "Reconciling".into(),
        //             secondary: None,
        //         })
        //         .await?;
        // }
        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "per.naess/v1alpha1",
            "kind": "Application",
            "status": ApplicationStatus {
                state: application_state,
                deployed: should_deploy
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = apps.patch_status(&name, &ps, &new_status).await?;

        // If no events were recieved, check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    // reconcile with finalize cleanup(object was deleted)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, kube::Error> {
        let client = ctx.client.clone();
        ctx.diagnostics.write().await.last_event = Utc::now();
        let reporter = ctx.diagnostics.read().await.reporter.clone();
        let recorder = Recorder::new(client.clone(), reporter, self.object_ref(&()));

        let ns = self.namespace().unwrap();
        cleanup_deployment(&self.spec, &ns, client.clone()).await?;

        recorder
            .publish(Event { 
                type_: EventType::Normal, 
                reason: "DeleteApplication".into(), 
                note: Some(format!("Delete `{}`", self.name_any())), 
                action: "Reconciling".into(), 
                secondary: None 
            })
            .await?;

        Ok(Action::await_change())
    }
}

/// Context for our reconciler
#[derive(Clone)]
struct Context {
    /// Kubernetes client
    client: Client,
    /// Diagnostics read by the web server
    diagnostics: Arc<RwLock<Diagnostics>>,
    /// Prometheus metrics
    metrics: Metrics,
}

#[instrument(skip(ctx, app), fields(trace_id))]
async fn reconcile(app: Arc<Application>, ctx: Arc<Context>) -> Result<Action, Error> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", &field::display(&trace_id));
    let start = Instant::now();
    ctx.metrics.reconciliations.inc();
    let client = ctx.client.clone();
    let name = app.name_any();
    let ns = app.namespace().unwrap();
    let apps: Api<Application> = Api::namespaced(client, &ns);

    let action = finalizer(&apps, CUSTOM_APP_FINALIZER, app, |event| async {
        match event {
           finalizer::Event::Apply(app) =>  app.reconcile(ctx.clone()).await,
           finalizer::Event::Cleanup(app) => app.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(Error::FinalizerError);

    let duration = start.elapsed().as_millis() as f64 / 1000.0;
    ctx.metrics
        .reconcile_duration
        .with_label_values(&[])
        .observe(duration);

    info!("Reconciled Application \"{}\" in {}", name, ns);
    action
}

async fn handle_deployment(app: &Application, ns: &str, client: Client, recorder: Recorder, name: &str) -> Result<(), kube::Error> {
    let should_deploy = app.spec.deploy;
    if app.was_deployed() && should_deploy {
        create_deployment(&app.spec, ns, client).await?;
        recorder.publish(Event { 
            type_: EventType::Normal, 
            reason: "CreatingDeployment".into(), 
            note: Some(format!("Creating deployment `{}`", name)), 
            action: "Reconciling".into(), 
            secondary: None, 
        })
        .await?;
    } else if app.was_deployed() && !should_deploy {
        cleanup_deployment(&app.spec, &ns, client).await?;
        recorder.publish(Event { 
            type_: EventType::Normal, 
            reason: "DeletingDeployment".into(), 
            note: Some(format!("Deleting deployment `{}`", name)), 
            action: "Reconciling".into(), 
            secondary: None, 
        })
        .await?;
    }

    Ok(())
}

// Prometheus metrics exposed on /metrics
#[derive(Clone)]
pub struct Metrics {
    pub reconciliations: IntCounter,
    pub failures: IntCounter,
    pub reconcile_duration: HistogramVec,
}

impl Metrics {
    fn new() -> Self {
        let reconcile_histogram = register_histogram_vec!(
            "app_controller_reconcile_duration_seconds",
            "The duration of reconcile to complete in seconds",
            &[],
            vec![0.01, 0.1, 0.25, 0.5, 1., 5., 15., 60.]
        )
        .unwrap();

        Metrics { 
            reconciliations: register_int_counter!("app_controller_reconciliations_total", "reconciliations").unwrap(), 
            failures: register_int_counter!(
                "app_controller_reconciliation_errors_total",
                "reconciliation errors"
            ).unwrap(), 
            reconcile_duration: reconcile_histogram 
        }
    }
}

// Diagnostics to be exposed on webserver
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc> ,
    #[serde(skip)]
    pub reporter: Reporter,
}

impl Diagnostics {
    fn new() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "app-reporter".into()
        }
    }
}

/// Data owned by the Operator
#[derive(Clone)]
pub struct Operator {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
}

fn error_policy(error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.failures.inc();
    Action::requeue(Duration::from_secs(5 * 60))
}

/// Operator that owns a Controller for Application
impl Operator {
    /// Lifecycle initialization interface for app
    ///
    /// This returns a `Operator` that drives a `Controller` + a future to be awaited
    /// It is up to `main` to wait for the controller stream
    pub async fn new() -> (Self, BoxFuture<'static, ()>) {
        let client = Client::try_default().await.expect("Create Client");
        let metrics = Metrics::new();
        let diagnostics = Arc::new(RwLock::new(Diagnostics::new()));
        let context = Arc::new(Context {
            client: client.clone(),
            metrics: metrics.clone(),
            diagnostics: diagnostics.clone(),
        });

        let apps = Api::<Application>::all(client);
        //Ensure CRD is installed before loop-watching
        let _r = apps
            .list(&ListParams::default().limit(1))
            .await
            .expect("Is the crd installed? please run: cargo run --bin crdgen | kubectl apply -f -");

        // All good. Start controller and return its future.
        let controller = Controller::new(apps, ListParams::default())
            .run(reconcile, error_policy, context)
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(()))
            .boxed();

        
        (Self { diagnostics }, controller)
    }

    /// Metrics
    pub fn metrics(&self) -> Vec<MetricFamily> {
        default_registry().gather()
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }
}
