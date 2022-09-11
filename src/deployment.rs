use k8s_openapi::api::apps::v1::Deployment;
use kube::{api::{PostParams, DeleteParams}, ResourceExt, Client, Api, runtime::events::{Recorder, Event, EventType}}; 
use serde_json::json;
use tracing::info;

use crate::operator::ApplicationSpec;

pub enum ApplicationDeploymentState {
    Deployed,
    Failed
}

pub async fn create_deployment(application_spec: &ApplicationSpec, ns: &str, client: Client, recorder: &Recorder) -> Result<(), kube::Error> {
    info!("Creating deployment for {}", application_spec.name);
    let deployments: Api<Deployment> = Api::namespaced(client, ns);
    let deployment: Deployment = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": application_spec.name,
            "labels": {
                "app": "nginx"
            }
        },
        "spec": {
            "replicas": 2,
            "selector": {
                "matchLabels": {
                    "app": "nginx"
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": "nginx"
                    }
                },
                "spec": {
                    "containers": [{
                        "name": application_spec.name,
                        "image": application_spec.image
                    }]
                }
            }
        }
    })).expect("Something is wrong with the deployment");

    let pp = PostParams::default();
    match deployments.create(&pp, &deployment).await {
        Ok(o) => {
            let name = o.name_any();
            assert_eq!(deployment.name_any(), name);
            info!("Created deployment {}", application_spec.name)
        },
        Err(kube::Error::Api(ae)) => assert_eq!(ae.code, 409),
        Err(e) => return Err(e.into())
    };

    info!("Get a {} deployment", application_spec.name);
    if let Some(deployment) =  deployments.get_opt(&application_spec.name).await? {
        info!("Got deployment");
        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "RunningApplication".into(),
                note: Some(format!("Deployment complete `{}`", application_spec.name)),
                action: "Reconciling".into(),
                secondary: None,
            })
            .await?;
    } else {
        info!("This didnt work");
        recorder
            .publish(Event {
                type_: EventType::Warning,
                reason: "FailedApplication".into(),
                note: Some(format!("Deployment not starting `{}`", application_spec.name)),
                action: "Reconciling".into(),
                secondary: None,
            })
            .await?;
    }

    Ok(())
}

pub async fn cleanup_deployment(application_spec: &ApplicationSpec, ns: &str, client: Client, recorder: &Recorder) -> Result<(), kube::Error> {
    info!("Cleaning up deployment for {}", application_spec.name);

    let deployments: Api<Deployment> = Api::namespaced(client, ns);
    deployments.delete(&application_spec.name, &DeleteParams::default()).await?
        .map_left(|o| {
            info!("Deleting deployment: {:?}", o.status);
        })
        .map_right(|s| info!("Deleted deployment: {:?}", s));

    Ok(())
}
