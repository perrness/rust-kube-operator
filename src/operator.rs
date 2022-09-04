use std::{sync::Arc, time::Duration};

use chrono::DateTime;
use k8s_openapi::chrono::Utc;
use kube::{
    CustomResource, Client, 
    runtime::{
        events::{Recorder, Reporter, EventType, Event},
        controller::Action, finalizer,
    }, 
    ResourceExt, Api, Resource, api::{Patch, PatchParams}
};
use prometheus::{IntCounter, HistogramVec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{sync::RwLock, time::Instant};
use tracing::{instrument, info};

use crate::Error;

static CUSTOM_APP_FINALIZER: &str = "custom_apps.per.naess";

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

/// The status object of  `Document`
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
        // ctx.diagnostics.write().await.last_event = Utc::now();
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
                .await?
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
        let _0 = caps.patch(&name, &ps, &new_status).await?;

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

// Diagnostics to be exposed on webserver
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc> ,
    #[serde(skip)]
    pub reporter: Reporter,
}

/// Data owned by the Operator
#[derive(Clone)]
pub struct Operator {
    /// Diagnostics populated by the reconciler
}
