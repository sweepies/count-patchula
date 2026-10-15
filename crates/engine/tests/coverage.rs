//! Config coverage uses authored representative manifests, never upstream source
//! from the comparison subjects (particularly the clean-room excluded project).
use rustovate_config::Manager;
use rustovate_manager::{Files, Scanner};
use std::path::Path;

#[test]
fn every_example_has_implemented_dispatch_for_the_twelve_repo_matrix() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files=Files::from([
        ("package.json".into(),r#"{"dependencies":{"example":"1.0.0"}}"#.into()),
        (".github/workflows/ci.yml".into(),"jobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4.2.0\n".into()),
        ("mise.toml".into(),"[tools]\nnode = '24.0.0'\n".into()),
        ("Cargo.toml".into(),"[dependencies]\nserde = '1.0'\n".into()),
        ("docker-compose.yml".into(),"services:\n  db:\n    image: postgres:16.3-alpine\n".into()),
        ("go.mod".into(),"module example.org/module\ngo 1.24.0\ntoolchain go1.24.2\nrequire example.org/dep v1.2.3\n".into()),
        ("main.tf".into(),"terraform {\n required_providers {\n random = {\n source = \"hashicorp/random\"\n version = \"3.6.0\"\n }\n }\n}\nmodule \"iam\" {\n source = \"terraform-aws-modules/iam/aws\"\n version = \"5.0.0\"\n}\n".into()),
        ("requirements.txt".into(),"six==1.16.0\n".into()),
        ("apps/example/deploy.yaml".into(),"apiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      containers:\n        - name: web\n          image: nginx:1.26-alpine\n".into()),
        ("kustomization.yaml".into(),"apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nimages:\n  - name: alpine\n    newTag: 3.20\n".into()),
        ("values.yaml".into(),"image:\n  repository: nginx\n  tag: 1.26-alpine\n".into()),
        ("apps/example/oci.yaml".into(),"apiVersion: source.toolkit.fluxcd.io/v1\nkind: OCIRepository\nmetadata:\n  name: chart\nspec:\n  url: oci://ghcr.io/stefanprodan/charts/podinfo\n  ref:\n    tag: 6.7.1\n".into()),
    ]);
    let mut count = 0;
    for entry in std::fs::read_dir(root.join("examples")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|v| v != "pkl") {
            continue;
        }
        let config = rustovate_config::load(&path).unwrap();
        assert!(
            config
                .managers
                .iter()
                .all(|m| rustovate_manager::supported(*m))
        );
        let scanner = Scanner::new(&config).unwrap();
        let extraction = scanner
            .extract(&files)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(extraction.warnings.is_empty());
        let evaluator = rustovate_lookup::Evaluator::new(&config).unwrap();
        for dep in &extraction.dependencies {
            rustovate_lookup::scheme(dep).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            if evaluator.needs_lookup(dep) {
                let mut catalog = rustovate_datasource::Catalog::default();
                catalog.versions.push(rustovate_datasource::Version {
                    version: if dep.source == rustovate_config::Source::Go {
                        "v2.0.0"
                    } else if dep.source == rustovate_config::Source::Docker {
                        "2.0.0-alpine"
                    } else {
                        "2.0.0"
                    }
                    .into(),
                    published_at: None,
                    digest: None,
                    yanked: false,
                    deprecated: false,
                });
                evaluator
                    .propose(dep, &catalog)
                    .unwrap_or_else(|e| panic!("{} {}: {e}", path.display(), dep.name));
            }
        }
        let name = path.file_stem().unwrap().to_str().unwrap();
        let expected: &[Manager] = match name {
            "benchmark-batch-public" | "sandbox-automerge" => &[Manager::GithubActions],
            "fnox" => &[Manager::Cargo, Manager::Compose],
            "immich-go" => &[Manager::Go],
            "cafeteria-order-tracker" => &[Manager::Terraform],
            "maccrae-infra" => &[
                Manager::Kubernetes,
                Manager::Kustomize,
                Manager::Flux,
                Manager::HelmValues,
                Manager::Requirements,
            ],
            _ => &[Manager::Npm],
        };
        for manager in expected {
            assert!(
                extraction
                    .dependencies
                    .iter()
                    .any(|d| d.manager == *manager),
                "{name}: {manager:?}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 15);
}
