use std::{sync::Arc, time::Duration};

use chrono::DateTime;
use futures::{future::BoxFuture, FutureExt, StreamExt};
use k8s_openapi::chrono::Utc;
use kube::{
    CustomResource, Client, 
    runtime::{
        events::{Recorder, Reporter, EventType, Event},
        controller::Action, finalizer, Controller,
    }, 
    ResourceExt, Api, Resource, api::{Patch, PatchParams, ListParams}
};
use prometheus::{IntCounter, HistogramVec, register_histogram_vec, register_int_counter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{sync::RwLock, time::Instant};
use tracing::{instrument, info, warn};

use crate::Error;

static CUSTOM_APP_FINALIZER: &str = "customapps.per.naess";

/// Generate the Kubernetes wrapper struct "CustomApp" from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml(in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(kind = "CustomApp", group = "per.naess", version = "v1", namespaced)]
#[kube(status = "CustomAppStatus", shortname = "cap")]
pub struct CustomAppSpec {
    title: String,
    hide: bool,
    content: String
}

/// The status object of  `CustomApp`
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct CustomAppStatus {
    hidden: bool
}

impl CustomApp {
    fn was_hidden(&self) -> bool {
        self.status.as_ref().map(|s| s.hidden).unwrap_or(false)
    }

    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, kube::Error> {
        let client = ctx.client.clone();
        ctx.diagnostics.write().await.last_event = Utc::now();
        let reporter = ctx.diagnostics.read().await.reporter.clone();
        let recorder = Recorder::new(client.clone(), reporter, self.object_ref(&()));
        let name = self.name_any();
        let ns = self.namespace().unwrap();
        let caps: Api<CustomApp> = Api::namespaced(client, &ns);

        let should_hide = self.spec.hide;
        if self.was_hidden() && should_hide {
            // only send event first time
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "HiddenCustomApp".into(),
                    note: Some(format!("Hiding `{}`", name)),
                    action: "Reconciling".into(),
                    secondary: None,
                })
                .await?;
        }
        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "per.naess/v1",
            "kind": "CustomApp",
            "status": CustomAppStatus {
                hidden: should_hide,
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = caps.patch_status(&name, &ps, &new_status).await?;

        // If no events were recieved, check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    // reconcile with finalize cleanup(object was deleted)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, kube::Error> {
        let client = ctx.client.clone();
        ctx.diagnostics.write().await.last_event = Utc::now();
        let reporter = ctx.diagnostics.read().await.reporter.clone();
        let recorder = Recorder::new(client.clone(), reporter, self.object_ref(&()));

        recorder
            .publish(Event { 
                type_: EventType::Normal, 
                reason: "DeleteCustomApp".into(), 
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

#[instrument(skip(ctx, cap), fields(trace_id))]
async fn reconcile(cap: Arc<CustomApp>, ctx: Arc<Context>) -> Result<Action, Error> {
    let start = Instant::now();
    let client = ctx.client.clone();
    let name = cap.name_any();
    let ns = cap.namespace().unwrap();
    let caps: Api<CustomApp> = Api::namespaced(client, &ns);

    let action = finalizer(&caps, CUSTOM_APP_FINALIZER, cap, |event| async {
        match event {
           finalizer::Event::Apply(cap) =>  cap.reconcile(ctx.clone()).await,
           finalizer::Event::Cleanup(cap) => cap.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(Error::FinalizerError);

    info!("Reconciled CustomApp \"{}\" in {}", name, ns);
    action
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
            "cap_controller_reconcile_duration_seconds",
            "The duration of reconcile to complete in seconds",
            &[],
            vec![0.01, 0.1, 0.25, 0.5, 1., 5., 15., 60.]
        )
        .unwrap();

        Metrics { 
            reconciliations: register_int_counter!("cap_controller_reconciliations_total", "reconciliations").unwrap(), 
            failures: register_int_counter!(
                "cap_controller_reconciliation_errors_total",
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
            reporter: "cap-reporter".into()
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
    Action::requeue(Duration::from_secs(5 * 60))
}

/// Operator that owns a Controller for CustomApp
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

        let caps = Api::<CustomApp>::all(client);
        //Ensure CRD is installed before loop-watching
        let _r = caps
            .list(&ListParams::default().limit(1))
            .await
            .expect("Is the crd installed? please run: cargo run --bin crdgen | kubectl apply -f -");

        // All good. Start controller and return its future.
        let controller = Controller::new(caps, ListParams::default())
            .run(reconcile, error_policy, context)
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(()))
            .boxed();

        (Self { diagnostics }, controller)
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }
}
