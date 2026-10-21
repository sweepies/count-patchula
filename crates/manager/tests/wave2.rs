use patchula_config::{DependencyKind, Manager, Source};
use patchula_manager::{Files, Scanner};
fn extract(files: &Files) -> Vec<patchula_manager::Dependency> {
    let config = patchula_config::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/maccrae-infra.pkl"
    )))
    .unwrap();
    let result = Scanner::new(&config).unwrap().extract(files).unwrap();
    assert!(result.warnings.is_empty());
    for dep in &result.dependencies {
        let text = &files[&dep.file];
        assert!(dep.location.start < dep.location.end);
        assert!(
            text.is_char_boundary(dep.location.start) && text.is_char_boundary(dep.location.end)
        );
        assert!(dep.location.line > 0 && dep.location.column > 0);
    }
    result.dependencies
}
#[test]
fn cargo_workspaces_targets_renames_and_lock_resolution() {
    let files = Files::from([
        (
            "Cargo.toml".into(),
            r#"[workspace.dependencies]
serde = "1.0"
[dependencies]
renamed = { package = "actual", version = "^0.3" }
local = { path = "local" }
[target.'cfg(unix)'.build-dependencies]
cc = "1"
[dev-dependencies]
test = "2.0"
"#
            .into(),
        ),
        (
            "Cargo.lock".into(),
            r#"version = 4
[[package]]
name = "actual"
version = "0.3.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "actual"
version = "0.4.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "transitive"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#
            .into(),
        ),
        (
            "crates/member/Cargo.toml".into(),
            "[dependencies]\nserde.workspace = true\n".into(),
        ),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 5);
    assert!(deps.iter().all(|d| d.manager == Manager::Cargo));
    let renamed = deps.iter().find(|d| d.name == "renamed").unwrap();
    assert_eq!(renamed.package, "actual");
    assert_eq!(renamed.resolved.as_deref(), Some("0.3.9"));
    assert_eq!(
        deps.iter().find(|d| d.name == "cc").unwrap().kind,
        DependencyKind::Build
    );
    assert!(
        deps.iter()
            .filter(|d| d.file == "Cargo.toml")
            .all(|d| files[&d.file][d.location.start..d.location.end].contains(&d.current))
    );
}
#[test]
fn dockerfile_args_multistage_platform_and_compose() {
    let files=Files::from([
        ("Dockerfile".into(),"ARG ALPINE_VERSION=3.20\nFROM --platform=$BUILDPLATFORM alpine:${ALPINE_VERSION} AS build\nFROM build AS copied\nFROM \\\n  nginx:1.26-bookworm\nFROM scratch\n".into()),
        ("compose.yaml".into(),"services:\n  db:\n    image: 'postgres:16.3-alpine@sha256:abc'\n  build:\n    build: .\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert!(
        deps.iter()
            .any(|d| d.package == "alpine" && d.current == "3.20")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "nginx" && d.current == "1.26-bookworm")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "postgres" && d.digest.as_deref() == Some("sha256:abc"))
    );
}
#[test]
fn go_directives_requires_and_python_markers_hashes() {
    let files=Files::from([
        ("go.mod".into(),"module example.org/m\ngo 1.24.0\ntoolchain go1.24.2\nrequire (\n example.org/lib v1.2.3 // indirect\n)\nrequire example.org/other/v2 v2.4.0\nreplace example.org/lib => ../local\n".into()),
        ("requirements.txt".into(),"# spec fixture\nrequests[security]==2.31.0 ; python_version >= '3.8'\nDjango>=4.2,<5\nsix==1.16.0 --hash=sha256:abc\n-r extra.txt\n-e ../local\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 7, "{deps:?}");
    assert!(
        deps.iter()
            .any(|d| d.kind == DependencyKind::GoDirective && d.current == "1.24.0")
    );
    assert!(
        deps.iter()
            .any(|d| d.kind == DependencyKind::Toolchain && d.current == "1.24.2")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "django" && d.current == ">=4.2,<5")
    );
}
#[test]
fn terraform_providers_modules_and_lockfile() {
    let files=Files::from([
        ("main.tf".into(),r#"terraform {
  required_providers {
    aws = {
      source = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = ">= 3.0"
  }
}
module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
  version = "5.4.0"
}
module "local" {
  source = "./local"
}
"#.into()),
        (".terraform.lock.hcl".into(),"provider \"registry.terraform.io/hashicorp/aws\" {\n version = \"5.70.0\"\n hashes = [\"h1:abc\"]\n}\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "aws")
            .unwrap()
            .resolved
            .as_deref(),
        Some("5.70.0")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "terraform-aws-modules/vpc/aws"
                && d.kind == DependencyKind::Module)
    );
    assert!(
        deps.iter()
            .all(|d| files[&d.file][d.location.start..d.location.end].contains(&d.current))
    );
}
#[test]
fn kubernetes_kustomize_flux_and_values_locations() {
    let files=Files::from([
        ("apps/demo/deploy.yaml".into(),"apiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      initContainers:\n        - name: init\n          image: busybox:1.36\n      containers:\n        - name: app\n          image: ghcr.io/example/app:1.2.3\n---\napiVersion: v1\nkind: ConfigMap\ndata:\n  image: fake:1\n".into()),
        ("kustomization.yaml".into(),"apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nimages:\n  - name: nginx\n    newName: library/nginx\n    newTag: 1.26-alpine\nhelmCharts:\n  - name: podinfo\n    repo: https://stefanprodan.github.io/podinfo\n    version: 6.7.1\n".into()),
        ("apps/demo/source.yaml".into(),"apiVersion: source.toolkit.fluxcd.io/v1\nkind: HelmRepository\nmetadata:\n  name: podinfo\n  namespace: demo\nspec:\n  url: https://stefanprodan.github.io/podinfo\n".into()),
        ("apps/demo/release.yaml".into(),"apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nmetadata:\n  name: app\n  namespace: demo\nspec:\n  chart:\n    spec:\n      chart: podinfo\n      version: 6.7.1\n      sourceRef:\n        kind: HelmRepository\n        name: podinfo\n  values:\n    image:\n      repository: ghcr.io/example/custom\n      tag: 1.2.0\n".into()),
        ("apps/demo/oci.yaml".into(),"apiVersion: source.toolkit.fluxcd.io/v1\nkind: OCIRepository\nmetadata:\n  name: chart\nspec:\n  url: oci://ghcr.io/stefanprodan/charts/podinfo\n  ref:\n    tag: 6.7.1\n".into()),
        ("values.yaml".into(),"image:\n  registry: ghcr.io\n  repository: example/app\n  tag: 1.2.3\nsidecar:\n  image: alpine:3.20\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 9, "{deps:?}");
    assert!(deps.iter().any(|d| d.source == Source::Helm
        && d.package == "https://stefanprodan.github.io/podinfo|podinfo"));
    assert!(
        deps.iter()
            .any(|d| d.source == Source::Oci && d.package == "ghcr.io/stefanprodan/charts/podinfo")
    );
    assert!(deps.iter().all(|d| d.package != "fake"));
}

#[test]
fn yaml_locations_follow_paths_with_identical_values_flow_maps_and_unicode() {
    let text = "# café 🦀\napiVersion: apps/v1\nkind: Deployment\nmetadata: {annotations: {image: 'nginx:1.26'}}\nspec: {template: {spec: {containers: [{name: app, image: 'nginx:1.26'}]}}}\n";
    let files = Files::from([("deploy.yaml".into(), text.into())]);
    let deps = extract(&files);
    // Kubernetes images only belong to workload specs.
    let spec = deps
        .iter()
        .find(|d| d.location.pointer.contains("/spec/"))
        .unwrap();
    assert_eq!(
        &text[spec.location.start..spec.location.end],
        "'nginx:1.26'"
    );
    assert!(spec.location.start > text.find("spec:").unwrap());
}

#[test]
fn flux_chart_refs_still_extract_embedded_image_pins() {
    let files=Files::from([("apps/demo/release.yaml".into(),"apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nspec:\n  chartRef: {kind: OCIRepository, name: app}\n  values:\n    image: {repository: ghcr.io/example/app, tag: 1.2.3}\n".into())]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].manager, Manager::Flux);
    assert_eq!(deps[0].package, "ghcr.io/example/app");
}

#[test]
fn exact_requirements_and_go_pseudo_versions_retain_observed_identity() {
    let deps = extract(&Files::from([
        ("requirements.txt".into(), "widget==2.3.4\nrange>=2,<3\nwild==2.*\n".into()),
        ("go.mod".into(), "module example.org/demo\nrequire example.org/lib v0.0.0-20240102030405-abcdef123456\nrequire example.org/plain v1.2.3\n".into()),
    ]));
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "widget")
            .unwrap()
            .resolved
            .as_deref(),
        Some("2.3.4")
    );
    assert!(
        deps.iter()
            .filter(|d| matches!(d.name.as_str(), "range" | "wild"))
            .all(|d| d.resolved.is_none())
    );
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "example.org/lib")
            .unwrap()
            .digest
            .as_deref(),
        Some("abcdef123456")
    );
    assert!(
        deps.iter()
            .find(|d| d.name == "example.org/plain")
            .unwrap()
            .digest
            .is_none()
    );
}

#[test]
fn terraform_opentofu_locks_and_runtime_constraints_use_the_same_identity() {
    let files = Files::from([
        ("infra/versions.tf".into(), "terraform {\n required_version = \">= 1.8.0\"\n required_providers {\n widget = { source = \"example/widget\", version = \"~> 2.0\" }\n }\n}\n".into()),
        ("infra/.terraform.lock.hcl".into(), "provider \"registry.opentofu.org/example/widget\" {\n version = \"2.7.1\"\n}\n".into()),
    ]);
    let deps = extract(&files);
    let provider = deps.iter().find(|d| d.name == "widget").unwrap();
    assert_eq!(provider.resolved.as_deref(), Some("2.7.1"));
    let runtime = deps
        .iter()
        .find(|d| d.name == "hashicorp/terraform")
        .unwrap();
    assert_eq!(runtime.current, ">= 1.8.0");
    assert_eq!(runtime.source, Source::GithubReleases);
    assert_eq!(
        &files[&runtime.file][runtime.location.start..runtime.location.end],
        "\">= 1.8.0\""
    );
}

#[test]
fn mise_git_tags_use_github_without_the_selector_prefix() {
    let deps = extract(&Files::from([(
        "mise.toml".into(),
        "[tools]\n\"cargo:https://github.com/example/tool\" = \"tag:v0.3.1\"\n".into(),
    )]));
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].source, Source::GithubTags);
    assert_eq!(deps[0].package, "example/tool");
    assert_eq!(deps[0].current, "v0.3.1");
    assert_eq!(deps[0].resolved.as_deref(), Some("v0.3.1"));
}

#[test]
fn setup_inputs_have_their_own_editable_locations() {
    let text = "jobs:\n  check:\n    steps:\n      - uses: actions/setup-node@v4\n        with: {node-version: '20'}\n      - uses: golangci/golangci-lint-action@v6\n        with:\n          version: v2.3\n      - uses: actions/setup-go@v5\n        with:\n          go-version-file: go.mod\n      - uses: actions/setup-node@v4\n        with:\n          node-version: ${{ matrix.node }}\n";
    let deps = extract(&Files::from([(
        ".github/workflows/ci.yml".into(),
        text.into(),
    )]));
    let tools: Vec<_> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::Tool)
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].package, "actions/node-versions");
    assert_eq!(
        &text[tools[0].location.start..tools[0].location.end],
        "'20'"
    );
    assert_eq!(tools[1].name, "golangci/golangci-lint");
    assert_eq!(
        &text[tools[1].location.start..tools[1].location.end],
        "v2.3"
    );
}

#[test]
fn setup_bun_inputs_are_npm_tools_with_concrete_locations() {
    let text = "jobs:\n  check:\n    steps:\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: latest\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: '1.2.3'\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: ${{ matrix.bun }}\n";
    let deps = extract(&Files::from([(
        ".github/workflows/ci.yml".into(),
        text.into(),
    )]));
    let tools: Vec<_> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::Tool)
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "bun");
    assert_eq!(tools[0].source, Source::Npm);
    assert!(tools[0].resolved.is_none());
    assert_eq!(tools[1].resolved.as_deref(), Some("1.2.3"));
    assert_eq!(
        &text[tools[1].location.start..tools[1].location.end],
        "'1.2.3'"
    );
}

#[test]
fn generated_flux_bundle_and_configmap_image_variables_are_concrete_dependencies() {
    let text = "# This manifest was generated by flux. DO NOT EDIT.\n# Flux Version: v2.3.4\napiVersion: v1\nkind: ConfigMap\ndata:\n  CONTROLLER_IMAGE: example/controller:1.2.3\n  NOT_AN_IMAGE: ''\n  image: arbitrary:1\n";
    let deps = extract(&Files::from([(
        "clusters/demo/bundle.yaml".into(),
        text.into(),
    )]));
    assert_eq!(deps.len(), 2);
    assert!(deps.iter().any(|d| d.manager == Manager::Flux
        && d.package == "fluxcd/flux2"
        && d.current == "v2.3.4"));
    assert!(
        deps.iter()
            .any(|d| d.manager == Manager::HelmValues && d.package == "example/controller")
    );
    assert!(deps.iter().all(|d| d.package != "arbitrary"));
}
