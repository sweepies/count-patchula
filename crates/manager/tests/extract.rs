use rustovate_config::{DependencyKind, Manager};
use rustovate_manager::{Files, Scanner};
fn config() -> rustovate_config::Config {
    rustovate_config::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/renovate-runner.pkl"
    )))
    .unwrap()
}
#[test]
fn manifests_workspaces_aliases_overrides_and_lock_resolution() {
    let files=Files::from([
        ("package.json".into(),r#"{"workspaces":["packages/*"],"dependencies":{"foo":"^1.0.0"},"devDependencies":{"alias":"npm:real@~2.0.0"},"peerDependencies":{"peer":"1.x"},"optionalDependencies":{"opt":"3.0.0"},"overrides":{"parent":{"foo":"4.0.0"}},"packageManager":"bun@1.2.3"}"#.into()),
        ("packages/a/package.json".into(),r#"{"dependencies":{"foo":"^1.0.0","local":"workspace:*"}}"#.into()),
        ("bun.lock".into(),r#"{// Bun JSONC
          "packages":{"foo":["foo@1.8.0", "", {}, "sha512-test"],"alias":["real@2.1.0", "", {}],},
        }"#.into()),
    ]);
    let scanner = Scanner::new(&config()).unwrap();
    assert!(scanner.needs("packages/a/package.json"));
    assert!(!scanner.needs("node_modules/foo/package.json"));
    let extraction = scanner.extract(&files).unwrap();
    assert_eq!(extraction.dependencies.len(), 7);
    let alias = extraction
        .dependencies
        .iter()
        .find(|d| d.name == "alias")
        .unwrap();
    assert_eq!(alias.package, "real");
    assert_eq!(alias.resolved.as_deref(), Some("2.1.0"));
    assert_eq!(alias.kind, DependencyKind::Development);
    for dep in &extraction.dependencies {
        let text = &files[&dep.file];
        assert!(dep.location.start < dep.location.end);
        assert!(text[dep.location.start..dep.location.end].contains(&dep.current));
    }
    let workspace = extraction
        .dependencies
        .iter()
        .find(|d| d.file.starts_with("packages/"))
        .unwrap();
    assert_eq!(workspace.resolved.as_deref(), Some("1.8.0"));
}
#[test]
fn workflows_composites_pins_containers_and_local_refs() {
    let files = Files::from([
        (
            ".github/workflows/test.yml".into(),
            r#"jobs:
  reusable:
    uses: org/repo/.github/workflows/build.yml@main
  build:
    container: node:22
    services:
      db:
        image: postgres:16
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4.2.0
      - uses: docker://alpine:3.20
      - uses: ./local
      - run: |
          echo 'uses: wrong/repo@v99'
"#
            .into(),
        ),
        (
            ".github/actions/local/action.yml".into(),
            "runs:\n  using: composite\n  steps:\n    - uses: org/action@v1\n".into(),
        ),
    ]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 6);
    assert!(!deps.iter().any(|d| d.package == "wrong/repo"));
    let pin = deps.iter().find(|d| d.name == "actions/checkout").unwrap();
    assert_eq!(pin.resolved.as_deref(), Some("v4.2.0"));
    assert_eq!(pin.current, "v4.2.0");
    assert_eq!(pin.digest.as_ref().unwrap().len(), 40);
    assert_eq!(
        deps.iter().find(|d| d.current == "main").unwrap().kind,
        DependencyKind::ReusableWorkflow
    );
}

#[test]
fn action_comment_digest_only_and_ref_models_keep_edit_format() {
    let text = "jobs:\n  a:\n    steps:\n      - uses: 'org/action/path@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' #  v1.2.3 trailing note\n      - uses: org/plain@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n      - uses: org/ref@main\n";
    let files = Files::from([(".github/workflows/test.yml".into(), text.into())]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps[0].current, "v1.2.3");
    assert_eq!(
        deps[0].digest.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        rustovate_manager::action_replacement(&deps[0], text, "v2.0.0", Some(&"c".repeat(40)))
            .unwrap(),
        format!(
            "'org/action/path@{}' #  v2.0.0 trailing note",
            "c".repeat(40)
        )
    );
    assert_eq!(deps[1].current, "b".repeat(40));
    assert!(deps[1].resolved.is_none());
    assert_eq!(deps[2].current, "main");
    assert!(deps[2].digest.is_none());
}

#[test]
fn hosted_runner_literals_preserve_families_and_skip_self_hosted_and_expressions() {
    let text = "jobs:\n  a:\n    runs-on: ubuntu-latest\n  b:\n    runs-on: [macos-15, windows-2025, ubuntu-24.04-arm]\n  c:\n    runs-on:\n      - macos-26-intel\n  d:\n    runs-on: [self-hosted, ubuntu-22.04]\n  e:\n    runs-on: ${{ matrix.os }}\n  f:\n    runs-on: custom-linux\n";
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&Files::from([(
            ".github/workflows/test.yml".into(),
            text.into(),
        )]))
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 5);
    assert!(deps.iter().all(|d| d.kind == DependencyKind::Runner));
    assert_eq!(deps[0].name, "ubuntu");
    assert_eq!(deps[0].current, "latest");
    assert!(deps[0].resolved.is_none());
    assert_eq!(deps[3].package, "ubuntu-arm");
    assert_eq!(deps[3].current, "24.04");
    assert_eq!(deps[4].package, "macos-intel");
    for dep in &deps {
        assert!(text[dep.location.start..dep.location.end].contains(&dep.current));
    }
}

#[test]
fn hosted_runner_locations_do_not_select_identical_self_hosted_labels_or_run_text() {
    let text = "'jobs': # workflows\n  own:\n    runs-on: [self-hosted, ubuntu-22.04]\n  hosted: # GitHub hosted job\n    steps:\n      - run: echo ubuntu-22.04\n    \"runs-on\": ubuntu-22.04\n";
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&Files::from([(
            ".github/workflows/test.yml".into(),
            text.into(),
        )]))
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].location.start, text.rfind("ubuntu-22.04").unwrap());
    assert_eq!(
        rustovate_manager::runner_replacement(&deps[0], text, "24.04").unwrap(),
        "ubuntu-24.04"
    );
}
#[test]
fn mise_forms_and_custom_capture_locations() {
    let files=Files::from([("mise.toml".into(),"[tools]\nbun = \"1.2.3\"\nnode = [\"20\", \"22\"]\n\"github:org/tool\" = { version = \"v2.0.0\", os = [\"linux\"] }\n".into()),(".tool-versions".into(),"bun 1.2.3 1.3.0 # comment\n".into()),("infra/versions.ts".into(),"// renovate: datasource=npm depName=renovate\nexport const RENOVATE_VERSION = \"44.0.0\";\n".into())]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 7);
    assert!(deps.iter().any(|d| d.manager == Manager::Regex));
    for dep in &deps {
        assert!(files[&dep.file][dep.location.start..dep.location.end].contains(&dep.current));
    }
}

#[test]
fn npm_is_strict_json_and_bun_lock_is_jsonc() {
    let scanner = Scanner::new(&config()).unwrap();
    for text in [
        "{'dependencies':{'a':'1.0.0'}}",
        "{\"dependencies\":{\"a\":\"1.0.0\",}}",
        "{\"dependencies\":{\"a\":\"1.0.0\" \"b\":\"2.0.0\"}}",
    ] {
        assert!(
            scanner
                .extract(&Files::from([("package.json".into(), text.into())]))
                .is_err()
        );
    }
}
#[test]
fn catalogs_and_node_version_files_are_npm_dependencies() {
    let mut config = config();
    config.managers = vec![Manager::Npm];
    let scanner = Scanner::new(&config).unwrap();
    for path in [
        "pnpm-workspace.yaml",
        ".yarnrc.yml",
        ".nvmrc",
        "web/.node-version",
    ] {
        assert!(scanner.needs(path), "{path}");
    }
    let files = Files::from([
        (
            "pnpm-workspace.yaml".into(),
            "packages: [packages/*]\ncatalog:\n  zod: 3.23.8\n  local: link:../x\ncatalogs:\n  legacy:\n    hono: 4.6.0\n".into(),
        ),
        (".nvmrc".into(), "lts/iron\n".into()),
        ("web/.node-version".into(), "v22.1.0\n".into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let names: Vec<_> = deps
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str(), d.current.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("pnpm-workspace.yaml", "zod", "3.23.8"),
            ("pnpm-workspace.yaml", "hono", "4.6.0"),
            ("web/.node-version", "node", "22.1.0"),
        ],
        "aliases like lts/iron and local links are not registry versions"
    );
    let node = deps.iter().find(|d| d.name == "node").unwrap();
    assert_eq!(node.kind, DependencyKind::Toolchain);
    assert_eq!(node.resolved.as_deref(), Some("22.1.0"));
}
#[test]
fn pyproject_tables_lock_resolutions_and_python_version() {
    let mut config = config();
    config.managers = vec![Manager::Pyproject];
    let scanner = Scanner::new(&config).unwrap();
    for path in [
        "pyproject.toml",
        "uv.lock",
        "poetry.lock",
        "svc/.python-version",
    ] {
        assert!(scanner.needs(path), "{path}");
    }
    let pyproject = r#"[project]
name = "demo"
dependencies = [
  "httpx>=0.27.0",
  "Pydantic_Core[email] (==2.8.0) ; python_version >= '3.12'",
  "local @ file:///tmp/x",
  "anyio",
  "escaped\u0020>=1",
]
[project.optional-dependencies]
cli = ['click~=8.1.0']
[dependency-groups]
dev = ["pytest>=8.0", {include-group = "lint"}]
lint = ["ruff==0.6.0"]
[tool.uv]
constraint-dependencies = ["idna<4"]
[tool.poetry.dependencies]
python = "^3.12"
rich = "^13.7"
attrs = { version = "~23.2", extras = ["x"] }
gitdep = { git = "https://example.test/x.git" }
any = "*"
[tool.poetry.group.test.dependencies]
coverage = "7.6.0"
"#;
    let lock = "version = 1\n[[package]]\nname = \"httpx\"\nversion = \"0.27.2\"\n[[package]]\nname = \"pydantic-core\"\nversion = \"2.8.0\"\n";
    let files = Files::from([
        ("pyproject.toml".into(), pyproject.into()),
        ("uv.lock".into(), lock.into()),
        ("svc/.python-version".into(), "3.12.4\n".into()),
        ("web/.python-version".into(), "pypy3.10\n".into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| {
            (
                d.package.as_str(),
                d.current.as_str(),
                d.resolved.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("httpx", ">=0.27.0", Some("0.27.2")),
            ("pydantic-core", "==2.8.0", Some("2.8.0")),
            ("click", "~=8.1.0", None),
            ("pytest", ">=8.0", None),
            ("ruff", "==0.6.0", None),
            ("idna", "<4", None),
            ("rich", "^13.7", None),
            ("attrs", "~23.2", None),
            ("coverage", "7.6.0", None),
            ("python/cpython", "3.12.4", Some("3.12.4")),
        ]
    );
    for dep in &deps {
        let text = &files[&dep.file];
        let slice = &text[dep.location.start..dep.location.end];
        // Poetry constraints are whole TOML strings; PEP 508 locations are the
        // specifier inside a string.
        if dep.location.pointer.starts_with("/tool/poetry/") {
            assert_eq!(slice.trim_matches(['"', '\'']), dep.current, "{}", dep.name);
        } else {
            assert_eq!(slice, dep.current, "{}", dep.name);
        }
    }
    let poetry = deps.iter().find(|d| d.name == "rich").unwrap();
    assert_eq!(poetry.scheme, rustovate_config::Scheme::Poetry);
    let groups = deps.iter().find(|d| d.name == "ruff").unwrap();
    assert_eq!(groups.kind, DependencyKind::Development);
}
#[test]
fn pre_commit_revs_use_github_or_forge_tags() {
    let mut config = config();
    config.managers = vec![Manager::PreCommit];
    let scanner = Scanner::new(&config).unwrap();
    assert!(scanner.needs(".pre-commit-config.yaml"));
    let text = r#"repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v5.0.0
    hooks: [{id: trailing-whitespace}]
  - repo: https://gitlab.com/grp/sub/hooks.git
    rev: "1.2.0"
  - repo: https://codeberg.org/owner/hooks
    rev: v0.3
  - repo: https://github.com/frozen/hooks
    rev: 0123456789abcdef0123456789abcdef01234567
  - repo: https://git.example.com/a/b
    rev: v1.0.0
  - repo: local
    hooks: [{id: mine}]
"#;
    let files = Files::from([(".pre-commit-config.yaml".into(), text.into())]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| (d.source, d.package.as_str(), d.current.as_str()))
        .collect();
    use rustovate_config::Source;
    assert_eq!(
        got,
        [
            (Source::GithubTags, "pre-commit/pre-commit-hooks", "v5.0.0"),
            (Source::ForgeTags, "gitlab.com/grp/sub/hooks", "1.2.0"),
            (Source::ForgeTags, "codeberg.org/owner/hooks", "v0.3"),
        ]
    );
    for dep in &deps {
        let slice = &text[dep.location.start..dep.location.end];
        assert_eq!(slice.trim_matches('"'), dep.current);
    }
}
#[test]
fn toolchain_files_charts_and_devcontainers() {
    let mut config = config();
    config.managers = vec![
        Manager::Cargo,
        Manager::Terraform,
        Manager::Helm,
        Manager::Devcontainer,
    ];
    let scanner = Scanner::new(&config).unwrap();
    let files = Files::from([
        ("rust-toolchain.toml".into(), "[toolchain]\nchannel = \"1.82.0\"\ncomponents = [\"clippy\"]\n".into()),
        ("tools/rust-toolchain".into(), "nightly-2026-01-01\n".into()),
        (".terraform-version".into(), "v1.14.7\n".into()),
        ("infra/.terraform-version".into(), "latest\n".into()),
        (
            "chart/Chart.yaml".into(),
            "apiVersion: v2\nname: app\ndependencies:\n  - name: redis\n    version: \"^20.0.0\"\n    repository: https://charts.bitnami.com/bitnami\n  - name: pg\n    version: 1.2.3\n    repository: oci://registry-1.docker.io/bitnamicharts/\n  - name: local\n    version: 0.1.0\n    repository: file://../local\n".into(),
        ),
        (
            ".devcontainer/devcontainer.json".into(),
            "{\n  // comment\n  \"image\": \"mcr.microsoft.com/devcontainers/base:1.2-bookworm\",\n  \"features\": {\n    \"ghcr.io/devcontainers/features/node:1\": {\"version\": \"22\"},\n    \"./local-feature\": {},\n  },\n}\n".into(),
        ),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| (d.file.as_str(), d.package.as_str(), d.current.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (
                ".devcontainer/devcontainer.json",
                "mcr.microsoft.com/devcontainers/base",
                "1.2-bookworm"
            ),
            (
                ".devcontainer/devcontainer.json",
                "ghcr.io/devcontainers/features/node",
                "1"
            ),
            (".terraform-version", "hashicorp/terraform", "1.14.7"),
            (
                "chart/Chart.yaml",
                "https://charts.bitnami.com/bitnami|redis",
                "^20.0.0"
            ),
            (
                "chart/Chart.yaml",
                "registry-1.docker.io/bitnamicharts/pg",
                "1.2.3"
            ),
            ("rust-toolchain.toml", "rust-lang/rust", "1.82.0"),
        ]
    );
    for dep in &deps {
        let slice = &files[&dep.file][dep.location.start..dep.location.end];
        assert!(
            slice.trim_matches('"') == dep.current || slice.ends_with(&format!(":{}", dep.current)),
            "{} {slice:?}",
            dep.name
        );
    }
}
#[test]
fn maven_poms_and_gradle_catalogs_scripts_and_wrappers() {
    let mut config = config();
    config.managers = vec![Manager::Maven, Manager::Gradle];
    let scanner = Scanner::new(&config).unwrap();
    let pom = r#"<?xml version="1.0"?>
<project>
  <parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.3.0</version></parent>
  <properties>
    <jackson.version>2.17.0</jackson.version>
  </properties>
  <!-- <dependency><groupId>x</groupId><artifactId>y</artifactId><version>9</version></dependency> -->
  <dependencies>
    <dependency>
      <groupId>com.fasterxml.jackson.core</groupId>
      <artifactId>jackson-databind</artifactId>
      <version>${jackson.version}</version>
      <exclusions><exclusion><groupId>a</groupId><artifactId>b</artifactId></exclusion></exclusions>
    </dependency>
    <dependency>
      <groupId>com.fasterxml.jackson.core</groupId>
      <artifactId>jackson-core</artifactId>
      <version>${jackson.version}</version>
    </dependency>
    <dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId><version> 33.4.8-jre </version></dependency>
    <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>
  </dependencies>
  <build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId><version>3.13.0</version></plugin></plugins></build>
</project>
"#;
    let catalog = r#"[versions]
kotlin = "2.0.0"
agp = "8.5.0"
[libraries]
core = { module = "androidx.core:core-ktx", version = "1.13.1" }
okhttp = "com.squareup.okhttp3:okhttp:4.12.0"
stdlib = { group = "org.jetbrains.kotlin", name = "kotlin-stdlib", version.ref = "kotlin" }
reflect = { module = "org.jetbrains.kotlin:kotlin-reflect", version.ref = "kotlin" }
dynamic = "com.example:dyn:1.+"
[plugins]
android = { id = "com.android.application", version.ref = "agp" }
"#;
    let script = "plugins {\n    id(\"org.jetbrains.kotlin.jvm\") version \"2.0.0\"\n}\ndependencies {\n    implementation(\"io.ktor:ktor-client-core:2.3.12\")\n    implementation(\"x:y:$v\")\n}\n";
    let files = Files::from([
        ("pom.xml".into(), pom.into()),
        ("gradle/libs.versions.toml".into(), catalog.into()),
        ("app/build.gradle.kts".into(), script.into()),
        (
            "gradle/wrapper/gradle-wrapper.properties".into(),
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-8.10-bin.zip\n"
                .into(),
        ),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| (d.file.as_str(), d.package.as_str(), d.current.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("app/build.gradle.kts", "io.ktor:ktor-client-core", "2.3.12"),
            (
                "app/build.gradle.kts",
                "https://plugins.gradle.org/m2|org.jetbrains.kotlin.jvm:org.jetbrains.kotlin.jvm.gradle.plugin",
                "2.0.0"
            ),
            (
                "gradle/libs.versions.toml",
                "androidx.core:core-ktx",
                "1.13.1"
            ),
            (
                "gradle/libs.versions.toml",
                "com.squareup.okhttp3:okhttp",
                "4.12.0"
            ),
            (
                "gradle/libs.versions.toml",
                "org.jetbrains.kotlin:kotlin-stdlib",
                "2.0.0"
            ),
            (
                "gradle/libs.versions.toml",
                "https://plugins.gradle.org/m2|com.android.application:com.android.application.gradle.plugin",
                "8.5.0"
            ),
            (
                "gradle/wrapper/gradle-wrapper.properties",
                "gradle/gradle-distributions",
                "8.10"
            ),
            (
                "pom.xml",
                "org.springframework.boot:spring-boot-starter-parent",
                "3.3.0"
            ),
            (
                "pom.xml",
                "com.fasterxml.jackson.core:jackson-databind",
                "2.17.0"
            ),
            ("pom.xml", "com.google.guava:guava", "33.4.8-jre"),
            (
                "pom.xml",
                "org.apache.maven.plugins:maven-compiler-plugin",
                "3.13.0"
            ),
        ]
    );
    for dep in &deps {
        assert_eq!(
            &files[&dep.file][dep.location.start..dep.location.end],
            dep.current,
            "{}",
            dep.name
        );
    }
    // A pinned checksum is refreshed by the lockfile runner, not skipped.
    let pinned = Files::from([(
        "gradle/wrapper/gradle-wrapper.properties".into(),
        "distributionSha256Sum=abc\ndistributionUrl=https\\://services.gradle.org/distributions/gradle-8.10-bin.zip\n".into(),
    )]);
    assert_eq!(scanner.extract(&pinned).unwrap().dependencies.len(), 1);
}
#[test]
fn nuget_projects_central_versions_tools_and_sdk() {
    let mut config = config();
    config.managers = vec![Manager::Nuget];
    let scanner = Scanner::new(&config).unwrap();
    let project = r#"<Project Sdk="Microsoft.NET.Sdk">
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
    <PackageReference Include="Serilog">
      <Version>4.0.0</Version>
    </PackageReference>
    <PackageReference Include="FromProperty" Version="$(Shared)" />
    <PackageReference Include="Floating" Version="1.*" />
    <!-- <PackageReference Include="Commented" Version="1.0.0" /> -->
  </ItemGroup>
</Project>
"#;
    let files = Files::from([
        ("src/App/App.csproj".into(), project.into()),
        (
            "Directory.Packages.props".into(),
            "<Project><ItemGroup><PackageVersion Include=\"xunit\" Version=\"[2.9.0,3.0)\" /></ItemGroup></Project>".into(),
        ),
        (".config/dotnet-tools.json".into(), "{\"version\":1,\"tools\":{\"dotnet-ef\":{\"version\":\"9.0.0\",\"commands\":[\"dotnet-ef\"]}}}".into()),
        ("global.json".into(), "{\n  \"sdk\": { \"version\": \"9.0.100\", \"rollForward\": \"latestFeature\" }\n}\n".into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str(), d.current.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (".config/dotnet-tools.json", "dotnet-ef", "9.0.0"),
            ("Directory.Packages.props", "xunit", "[2.9.0,3.0)"),
            ("global.json", "dotnet-sdk", "9.0.100"),
            ("src/App/App.csproj", "Newtonsoft.Json", "13.0.3"),
            ("src/App/App.csproj", "Serilog", "4.0.0"),
        ]
    );
    for dep in &deps {
        assert_eq!(
            &files[&dep.file][dep.location.start..dep.location.end],
            dep.current,
            "{}",
            dep.name
        );
    }
}
