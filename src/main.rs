use std::thread;

use kube::{Client, api::{Api, ListParams, ResourceExt, PostParams, PatchParams, Patch, DeleteParams}, runtime::wait::{await_condition, conditions::is_pod_running}};
use k8s_openapi::api::core::v1::Pod;
use serde_json::json;
use tracing::info;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Tracing logs
    tracing_subscriber::fmt::init();

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    // Create pod in namepace
    let pods = Api::namespaced(client, "per-controller");
    let p: Pod = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "per-controller-pod",
            "labels": {
                "name": "per-controller-pod-label"
            }
        },
        "spec": {
            "containers": [{
                "name": "nginx",
                "image": "nginx:1.14.2",
                "ports": [{
                    "containerPort": 80
                }]
            }]
        }
    }))?; 

    let pp = PostParams::default();
    match pods.create(&pp, &p).await {
        Ok(o) => {
            let name = o.name_any();
            assert_eq!(p.name_any(), name);
            info!("Created {}", name);
        },
        Err(kube::Error::Api(ae)) => assert_eq!(ae.code, 409), // If you skipped delete, for instance
        Err(e) => return Err(e.into()),
    }

    // Watch the pod for a few seconds
    let establish = await_condition(pods.clone(), "per-controller-pod", is_pod_running());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), establish).await?;

    // Verify we can get it
    info!("Get per-controller-pod");
    let p1cpy = pods.get("per-controller-pod").await?;
    if let Some(spec) = &p1cpy.spec {
        info!("Got per-controller-pod with containers: {:?}", spec.containers);
        assert_eq!(spec.containers[0].name, "nginx");
    }

    // Replace its spec
    info!("Patch per-controller-pod");
    let patch = json!({
        "metadata": {
            "resourceVersion": p1cpy.resource_version(),
        },
        "spec": {
            "activeDeadlineSeconds": 5
        }
    });
    let patchparams = PatchParams::default();
    let p_patched = pods.patch("per-controller-pod", &patchparams, &Patch::Merge(&patch)).await?;
    assert_eq!(p_patched.spec.unwrap().active_deadline_seconds, Some(5));

    let lp = ListParams::default().fields(&format!("metadata.name={}", "per-controller-pod"));
    for p in pods.list(&lp).await? {
        info!("Found Pod: {}", p.name_any());
    }

    let dp = DeleteParams::default();
    pods.delete("per-controller-pod", &dp).await?.map_left(|pdel| {
        assert_eq!(pdel.name_any(), "per-controller-pod");
        info!("Deleting per-controller-pod started {:?}", pdel);
    });

    // Read pods in the configured namespace into the typed interface from k8s-openapi
    for p in pods.list(&ListParams::default()).await? {
        println!("found pod {}", p.name_any());
    }

    Ok(())
}
