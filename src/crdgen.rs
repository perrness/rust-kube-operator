use kube::CustomResourceExt;

fn main() {
    print!{"{}", serde_yaml::to_string(&operator::CustomApp::crd()).unwrap()}
}
