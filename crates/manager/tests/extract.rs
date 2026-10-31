use patchula_config::{DependencyKind, Manager};
use patchula_manager::{Files, Scanner};
fn config() -> patchula_config::Config {
    patchula_config::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/renovate-migration.pkl"
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
        patchula_manager::action_replacement(&deps[0], text, "v2.0.0", Some(&"c".repeat(40)))
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
        patchula_manager::runner_replacement(&deps[0], text, "24.04").unwrap(),
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
        let extraction = scanner
            .extract(&Files::from([("package.json".into(), text.into())]))
            .unwrap();
        assert!(extraction.dependencies.is_empty());
        assert!(extraction.warnings[0].starts_with("package.json: skipped:"));
    }
}
/// A file that does not parse, such as an ERB-templated YAML config, is
/// skipped with a warning while the rest of the repository is extracted.
#[test]
fn unparseable_files_are_skipped_with_a_warning() {
    let defaults = patchula_config::load_files(
        "/repo/patchula.pkl",
        std::collections::BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"".into(),
        )]),
    )
    .unwrap();
    let scanner = Scanner::new(&defaults).unwrap();
    let files = Files::from([
        (
            "config/email.yml".into(),
            "production:\n  smtp:\n    tls: <%= ENV.fetch('SMTP_TLS', false) == 'true' ? true : nil %>\n".into(),
        ),
        ("compose.yml".into(), "services:\n  db:\n    image: postgres:16.4\n".into()),
    ]);
    let extraction = scanner.extract(&files).unwrap();
    assert_eq!(
        extraction.dependencies.len(),
        1,
        "{:?}",
        extraction.dependencies
    );
    assert_eq!(extraction.dependencies[0].file, "compose.yml");
    assert_eq!(extraction.warnings.len(), 1, "{:?}", extraction.warnings);
    assert!(extraction.warnings[0].starts_with("config/email.yml: skipped:"));
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
            ("python", "3.12.4", Some("3.12.4")),
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
    assert_eq!(poetry.scheme, patchula_config::Scheme::Poetry);
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
    use patchula_config::Source;
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
            (".terraform-version", "terraform", "1.14.7"),
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
            ("rust-toolchain.toml", "rust", "1.82.0"),
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
            ("gradle/wrapper/gradle-wrapper.properties", "gradle", "8.10"),
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
#[test]
fn bundler_gems_lock_resolutions_and_ruby_pins() {
    let mut config = config();
    config.managers = vec![Manager::Bundler];
    let scanner = Scanner::new(&config).unwrap();
    let gemfile = r#"source "https://rubygems.org"
ruby "3.3.4"
gem "rails", "~> 7.1.0"
gem 'puma', '>= 6.0'
gem "pg", ">= 1.1", "< 2.0"
gem "mine", git: "https://example.test/mine.git"
gem "local", "1.0", path: "../local"
# gem "commented", "1.0"
group :test do
  gem "rspec-rails", "6.1.3"
end
"#;
    let lock = "GEM\n  remote: https://rubygems.org/\n  specs:\n    rails (7.1.3.4)\n      actionpack (= 7.1.3.4)\n    puma (6.4.2)\n    nokogiri (1.16.6-x86_64-linux)\n\nPLATFORMS\n  ruby\n";
    let files = Files::from([
        ("Gemfile".into(), gemfile.into()),
        ("Gemfile.lock".into(), lock.into()),
        ("api/.ruby-version".into(), "ruby-3.3.4\n".into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| {
            (
                d.file.as_str(),
                d.name.as_str(),
                d.current.as_str(),
                d.resolved.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Gemfile", "rails", "~> 7.1.0", Some("7.1.3.4")),
            ("Gemfile", "puma", ">= 6.0", Some("6.4.2")),
            ("Gemfile", "rspec-rails", "6.1.3", None),
            ("Gemfile", "ruby", "3.3.4", Some("3.3.4")),
            ("api/.ruby-version", "ruby", "3.3.4", Some("3.3.4")),
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
#[test]
fn composer_requirements_and_lock_resolutions() {
    let mut config = config();
    config.managers = vec![Manager::Composer];
    let scanner = Scanner::new(&config).unwrap();
    let manifest = r#"{
    "require": {
        "php": "^8.2",
        "ext-json": "*",
        "composer-runtime-api": "^2.2",
        "laravel/framework": "^11.9",
        "Guzzlehttp/Guzzle": "~7.8.1",
        "acme/branch": "dev-main",
        "acme/any": "*"
    },
    "require-dev": {
        "phpunit/phpunit": "11.2.5"
    }
}
"#;
    let lock = r#"{"packages": [{"name": "laravel/framework", "version": "v11.14.0"}, {"name": "guzzlehttp/guzzle", "version": "7.8.1"}], "packages-dev": [{"name": "phpunit/phpunit", "version": "11.2.5"}]}"#;
    let files = Files::from([
        ("composer.json".into(), manifest.into()),
        ("composer.lock".into(), lock.into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| {
            (
                d.package.as_str(),
                d.current.as_str(),
                d.resolved.as_deref(),
                d.kind,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "laravel/framework",
                "^11.9",
                Some("v11.14.0"),
                DependencyKind::Production
            ),
            (
                "guzzlehttp/guzzle",
                "~7.8.1",
                Some("7.8.1"),
                DependencyKind::Production
            ),
            (
                "phpunit/phpunit",
                "11.2.5",
                Some("11.2.5"),
                DependencyKind::Development
            ),
        ]
    );
    for dep in &deps {
        assert_eq!(&manifest[dep.location.start..dep.location.end], dep.current);
    }
}
/// Versions held in variables are found through their references and edited
/// at their single definition: ext blocks, def/val locals, extra properties,
/// map literals, indexed maps and gradle.properties.
#[test]
fn gradle_variables_resolve_to_their_definitions() {
    let mut config = config();
    config.managers = vec![Manager::Gradle];
    let scanner = Scanner::new(&config).unwrap();
    let root = r#"plugins {
  id 'org.owasp.dependencycheck' version '12.2.2'
  id "io.swagger.core.v3.swagger-gradle-plugin" version "${swaggerVersion}"
}
version = '4.5.0-SNAPSHOT'
ext {
    junitVersion = "4.13.2"
    guavaVersion = '33.7.2-jre'
}
def okioVersion = "3.9.0"
ext.set('slf4jVersion', '2.0.13')
// implementation "com.example:commented:$junitVersion"
/* implementation "com.example:block:1.0.0" */
dependencies {
    implementation "junit:junit:$junitVersion"
    implementation "com.google.guava:guava:${guavaVersion}"
    implementation "com.squareup.okio:okio:$okioVersion"
    implementation group: 'org.slf4j', name: 'slf4j-api', version: slf4jVersion
    implementation group: 'org.slf4j', name: 'slf4j-simple', version: '2.0.12'
    implementation "org.apache.kafka:kafka-clients:$version"
    implementation "io.swagger.core.v3:swagger-jaxrs2-jakarta:$swaggerVersion"
    implementation "com.example:twice:$twiceVersion"
    testImplementation "org.apache.kafka:kafka-streams:$versions.kafka_28"
    testImplementation "org.apache.kafka:kafka-streams:$versions.kafka_30"
    implementation "2020-07-01T12:34:56Z"
}
"#;
    let versions = r#"ext {
  versions = [:]
  libs = [:]
}
versions += [
  jackson: "2.15.0",
  checkstyle: project.hasProperty('checkstyleVersion') ? checkstyleVersion : "12.3.1",
  "jetty": '12.0.10',
  kafka_28: "2.8.2",
  kafka_30: "3.0.2",
]
versions["scala"] = "2.13.18"
libs += [
  jacksonDatabind: "com.fasterxml.jackson.core:jackson-databind:$versions.jackson",
  jacksonCore: "com.fasterxml.jackson.core:jackson-core:$versions.jackson",
  jetty: "org.eclipse.jetty:jetty-server:${versions.jetty}",
  scala: "org.scala-lang:scala-library:$versions.scala",
]
"#;
    let kotlin = r#"val ktorVersion = "2.3.12"
val twiceVersion = "1.0.0"
extra["coroutines"] = "1.8.1"
dependencies {
    implementation("io.ktor:ktor-client-core:$ktorVersion")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:${coroutines}")
    implementation("com.example:dotted:$ktorVersion.extra")
}
"#;
    let files = Files::from([
        ("build.gradle".into(), root.into()),
        ("gradle/dependencies.gradle".into(), versions.into()),
        ("app/build.gradle.kts".into(), kotlin.into()),
        (
            "app/src/test/resources/fixture.gradle".into(),
            "dependencies { implementation 'com.example:fixture:1.0.0' }\n".into(),
        ),
        (
            "buildSrc/src/main/groovy/conventions.gradle".into(),
            "dependencies { implementation 'com.example:convention:2.0.0' }\n".into(),
        ),
        (
            "gradle.properties".into(),
            "org.gradle.jvmargs=-Xmx4g\nswaggerVersion=2.2.55\ntwiceVersion=1.0.0\n".into(),
        ),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let mut got: Vec<_> = deps
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str(), d.current.as_str()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("app/build.gradle.kts", "io.ktor:ktor-client-core", "2.3.12"),
            (
                "app/build.gradle.kts",
                "org.jetbrains.kotlinx:kotlinx-coroutines-core",
                "1.8.1"
            ),
            ("build.gradle", "com.google.guava:guava", "33.7.2-jre"),
            ("build.gradle", "com.squareup.okio:okio", "3.9.0"),
            ("build.gradle", "junit:junit", "4.13.2"),
            (
                "build.gradle",
                "org.owasp.dependencycheck:org.owasp.dependencycheck.gradle.plugin",
                "12.2.2"
            ),
            ("build.gradle", "org.slf4j:slf4j-api", "2.0.13"),
            ("build.gradle", "org.slf4j:slf4j-simple", "2.0.12"),
            (
                "buildSrc/src/main/groovy/conventions.gradle",
                "com.example:convention",
                "2.0.0"
            ),
            (
                "gradle.properties",
                "io.swagger.core.v3.swagger-gradle-plugin:io.swagger.core.v3.swagger-gradle-plugin.gradle.plugin",
                "2.2.55"
            ),
            (
                "gradle/dependencies.gradle",
                "com.fasterxml.jackson.core:jackson-databind",
                "2.15.0"
            ),
            (
                "gradle/dependencies.gradle",
                "org.eclipse.jetty:jetty-server",
                "12.0.10"
            ),
            (
                "gradle/dependencies.gradle",
                "org.scala-lang:scala-library",
                "2.13.18"
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
}

#[test]
fn lockfile_only_registry_packages_are_listed_apart_from_dependencies() {
    let files = Files::from([
        (
            "package.json".into(),
            r#"{"dependencies":{"foo":"^1.0.0"}}"#.into(),
        ),
        (
            "package-lock.json".into(),
            r#"{"lockfileVersion":3,"packages":{
                "":{"dependencies":{"foo":"^1.0.0"}},
                "node_modules/foo":{"version":"1.2.0","resolved":"https://registry.npmjs.org/foo/-/foo-1.2.0.tgz"},
                "node_modules/foo/node_modules/bar":{"version":"2.0.0","resolved":"https://registry.npmjs.org/bar/-/bar-2.0.0.tgz"},
                "node_modules/linked":{"resolved":"packages/linked","link":true},
                "node_modules/local":{"version":"3.0.0","resolved":"file:../local"}
            }}"#
            .into(),
        ),
    ]);
    let extraction = Scanner::new(&config()).unwrap().extract(&files).unwrap();
    assert_eq!(extraction.dependencies.len(), 1);
    let locked: Vec<_> = extraction
        .locked
        .iter()
        .map(|l| (l.manager, l.package.as_str(), l.version.as_str()))
        .collect();
    assert_eq!(locked, [(Manager::Npm, "bar", "2.0.0")]);
    let bar = &extraction.locked[0];
    let text = &files["package-lock.json"];
    assert_eq!(&text[bar.location.start..bar.location.end], "\"2.0.0\"");
    let dep = bar.dependency();
    assert_eq!(dep.kind, DependencyKind::Transitive);
    assert_eq!(dep.file, "package-lock.json");
}

/// `aube-lock.yaml` is pnpm's v9 format: `importers` give each manifest's
/// resolved versions, `packages` every registry package beneath them.
#[test]
fn aube_lock_yaml_gives_direct_versions_and_lists_transitive_packages() {
    let lock = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      foo:\n        specifier: ^1.0.0\n        version: 1.2.0(peer@3.0.0)\n      '@scope/baz':\n        specifier: 2.0.0\n        version: 2.0.0\n    devDependencies:\n      aliased:\n        specifier: npm:string-width@^4\n        version: string-width@4.2.3\n\n  packages/app:\n    dependencies:\n      foo:\n        specifier: ^1.0.0\n        version: 1.1.0\n      shared:\n        specifier: workspace:*\n        version: link:../shared\n\npackages:\n\n  '@scope/baz@2.0.0':\n    resolution: {integrity: sha512-aaa}\n\n  bar@2.0.0:\n    resolution: {integrity: sha512-bbb}\n\n  foo@1.1.0:\n    resolution: {integrity: sha512-ccc}\n\n  foo@1.2.0:\n    resolution: {integrity: sha512-ddd}\n\n  gitdep@1.0.0:\n    resolution: {tarball: https://example.invalid/gitdep.tgz}\n\nsnapshots: {}\n";
    let files = Files::from([
        (
            "package.json".into(),
            r#"{"dependencies":{"foo":"^1.0.0","@scope/baz":"2.0.0"},"devDependencies":{"aliased":"npm:string-width@^4"}}"#.into(),
        ),
        (
            "packages/app/package.json".into(),
            r#"{"dependencies":{"foo":"^1.0.0","shared":"^1.0.0"}}"#.into(),
        ),
        ("aube-lock.yaml".into(), lock.into()),
    ]);
    // The engine reads only the files the scanner needs: the lock has to be one.
    let scanner = Scanner::new(&config()).unwrap();
    assert!(scanner.needs("aube-lock.yaml"));
    assert!(scanner.needs("docs/aube-lock.yaml"));
    let files: Files = files
        .into_iter()
        .filter(|(p, _)| scanner.needs(p))
        .collect();
    assert!(files.contains_key("aube-lock.yaml"));
    let extraction = scanner.extract(&files).unwrap();
    let resolved: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str(), d.resolved.as_deref()))
        .collect();
    assert!(
        resolved.contains(&("package.json", "foo", Some("1.2.0"))),
        "{resolved:?}"
    );
    assert!(
        resolved.contains(&("package.json", "@scope/baz", Some("2.0.0"))),
        "{resolved:?}"
    );
    assert!(
        resolved.contains(&("package.json", "aliased", Some("4.2.3"))),
        "{resolved:?}"
    );
    assert!(
        resolved.contains(&("packages/app/package.json", "foo", Some("1.1.0"))),
        "{resolved:?}"
    );
    // A workspace link is no locked version.
    assert!(
        resolved.contains(&("packages/app/package.json", "shared", None)),
        "{resolved:?}"
    );
    // Only packages no manifest resolves to are transitive; a registry
    // tarball without an integrity hash from the registry is not one.
    let locked: Vec<_> = extraction
        .locked
        .iter()
        .map(|l| (l.package.as_str(), l.version.as_str()))
        .collect();
    assert_eq!(locked, [("bar", "2.0.0")]);
    let bar = &extraction.locked[0];
    assert_eq!(bar.file, "aube-lock.yaml");
    assert_eq!(&lock[bar.location.start..bar.location.end], "bar@2.0.0");
    assert_eq!(bar.dependency().kind, DependencyKind::Transitive);
}

#[test]
fn woodpecker_steps_services_clone_anchors_and_variables() {
    let mut config = config();
    config.managers = vec![Manager::Woodpecker];
    let scanner = Scanner::new(&config).unwrap();
    for (path, needed) in [
        (".woodpecker.yml", true),
        (".woodpecker.yaml", true),
        (".woodpecker/test.yaml", true),
        (".woodpecker/release.yml", true),
        (".woodpecker/nested/deep.yml", false),
        ("sub/.woodpecker.yml", false),
        (".woodpecker/README.md", false),
        (".woodpecker.json", false),
    ] {
        assert_eq!(scanner.needs(path), needed, "{path}");
    }
    let files = Files::from([
        (
            ".woodpecker.yml".into(),
            "pipeline:\n  build:\n    image: golang:1.20\n  publish:\n    image: plugins/docker:20\nservices:\n  db:\n    image: postgres:15\n".into(),
        ),
        (
            ".woodpecker/list.yaml".into(),
            "clone:\n  git:\n    image: woodpeckerci/plugin-git:2.5.0\nmatrix:\n  GO: [\"1.21\", \"1.22\"]\nsteps:\n  - name: test\n    image: golang:${GO}\n  - name: env\n    image: ${REGISTRY}/tool:1.0\n  - name: pinned\n    image: alpine:3.19@sha256:6457d53fb065d6f250e1504b9bc42d5b6c65941d57532c072d929dd0628977d0\nservices:\n  - name: redis\n    image: redis:7\n".into(),
        ),
        (
            ".woodpecker/map.yml".into(),
            "variables:\n  - &golang 'docker.io/golang:1.22-alpine'\n  - &base\n    image: node:22\nsteps:\n  build:\n    image: *golang\n  vet:\n    image: *golang\n  web:\n    <<: *base\n    commands: [npm ci]\n  release:\n    image: woodpeckerci/plugin-release:0.2\n".into(),
        ),
    ]);
    let extraction = scanner.extract(&files).unwrap();
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            let text = &files[&d.file];
            (
                d.file.as_str(),
                d.package.as_str(),
                d.current.as_str(),
                d.digest.is_some(),
                &text[d.location.start..d.location.end],
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (".woodpecker.yml", "golang", "1.20", false, "golang:1.20"),
            (
                ".woodpecker.yml",
                "plugins/docker",
                "20",
                false,
                "plugins/docker:20"
            ),
            (".woodpecker.yml", "postgres", "15", false, "postgres:15"),
            (
                ".woodpecker/list.yaml",
                "woodpeckerci/plugin-git",
                "2.5.0",
                false,
                "woodpeckerci/plugin-git:2.5.0"
            ),
            (
                ".woodpecker/list.yaml",
                "alpine",
                "3.19",
                true,
                "alpine:3.19@sha256:6457d53fb065d6f250e1504b9bc42d5b6c65941d57532c072d929dd0628977d0"
            ),
            (".woodpecker/list.yaml", "redis", "7", false, "redis:7"),
            (
                ".woodpecker/map.yml",
                "docker.io/golang",
                "1.22-alpine",
                false,
                "'docker.io/golang:1.22-alpine'"
            ),
            (".woodpecker/map.yml", "node", "22", false, "node:22"),
            (
                ".woodpecker/map.yml",
                "woodpeckerci/plugin-release",
                "0.2",
                false,
                "woodpeckerci/plugin-release:0.2"
            ),
        ]
    );
    assert!(
        extraction
            .dependencies
            .iter()
            .all(|d| d.kind == DependencyKind::ContainerImage)
    );
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                Some(8),
                "golang:${GO}",
                "the image varies with the matrix variable GO"
            ),
            (
                Some(10),
                "${REGISTRY}/tool:1.0",
                "the image contains a variable"
            ),
        ]
    );
}
#[test]
fn forgejo_and_gitea_workflows_resolve_bare_refs_on_their_default_action_host() {
    use patchula_config::Source;
    let mut config = config();
    config.managers = vec![Manager::GithubActions];
    let scanner = Scanner::new(&config).unwrap();
    for path in [
        ".forgejo/workflows/ci.yml",
        ".gitea/workflows/ci.yaml",
        ".forgejo/actions/x/action.yml",
    ] {
        assert!(scanner.needs(path), "{path}");
    }
    assert!(!scanner.needs("sub/.forgejo/workflows/ci.yml"));
    let text = r#"jobs:
  test:
    runs-on: ubuntu-22.04
    container: data.forgejo.org/oci/node:24-trixie
    steps:
      - uses: actions/checkout@v6.0.3
      - uses: https://code.forgejo.org/actions/setup-forgejo@0191c04814b526caf141138a5567000ab08ddae0 # v3.2.0
      - uses: https://github.com/actions/cache@v4
      - uses: https://data.forgejo.org/actions/setup-go@v6
        with:
          go-version: 1.25.1
      - uses: https://git.example.com/owner/action@v1
"#;
    let files = Files::from([
        (".forgejo/workflows/ci.yml".into(), text.into()),
        (".gitea/workflows/ci.yml".into(), text.into()),
    ]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| {
            (
                d.file.as_str(),
                d.source,
                d.name.as_str(),
                d.package.as_str(),
                d.current.as_str(),
                d.digest.is_some(),
            )
        })
        .collect();
    let forgejo = ".forgejo/workflows/ci.yml";
    let gitea = ".gitea/workflows/ci.yml";
    assert_eq!(
        got,
        [
            (
                forgejo,
                Source::ForgeTags,
                "actions/checkout",
                "data.forgejo.org/actions/checkout",
                "v6.0.3",
                false
            ),
            (
                forgejo,
                Source::ForgeTags,
                "code.forgejo.org/actions/setup-forgejo",
                "code.forgejo.org/actions/setup-forgejo",
                "v3.2.0",
                true
            ),
            (
                forgejo,
                Source::GithubTags,
                "actions/cache",
                "actions/cache",
                "v4",
                false
            ),
            (
                forgejo,
                Source::ForgeTags,
                "data.forgejo.org/actions/setup-go",
                "data.forgejo.org/actions/setup-go",
                "v6",
                false
            ),
            (
                forgejo,
                Source::GithubReleases,
                "go",
                "actions/go-versions",
                "1.25.1",
                false
            ),
            (
                forgejo,
                Source::Docker,
                "data.forgejo.org/oci/node",
                "data.forgejo.org/oci/node",
                "24-trixie",
                false
            ),
            (
                gitea,
                Source::GithubTags,
                "actions/checkout",
                "actions/checkout",
                "v6.0.3",
                false
            ),
            (
                gitea,
                Source::ForgeTags,
                "code.forgejo.org/actions/setup-forgejo",
                "code.forgejo.org/actions/setup-forgejo",
                "v3.2.0",
                true
            ),
            (
                gitea,
                Source::GithubTags,
                "actions/cache",
                "actions/cache",
                "v4",
                false
            ),
            (
                gitea,
                Source::ForgeTags,
                "data.forgejo.org/actions/setup-go",
                "data.forgejo.org/actions/setup-go",
                "v6",
                false
            ),
            (
                gitea,
                Source::GithubReleases,
                "go",
                "actions/go-versions",
                "1.25.1",
                false
            ),
            (
                gitea,
                Source::Docker,
                "data.forgejo.org/oci/node",
                "data.forgejo.org/oci/node",
                "24-trixie",
                false
            ),
        ]
    );
    for dep in &deps {
        patchula_manager::edits(dep, &files[&dep.file], &dep.current, dep.digest.as_deref())
            .unwrap();
    }
    let pin = deps.iter().find(|d| d.name == "actions/checkout").unwrap();
    let edit =
        patchula_manager::action_replacement(pin, text, "v6.1.0", Some(&"d".repeat(40))).unwrap();
    assert_eq!(
        edit,
        format!("actions/checkout@{} # v6.1.0", "d".repeat(40))
    );
}
#[test]
fn gitlab_ci_images_services_components_and_project_includes() {
    use patchula_config::Source;
    let mut config = config();
    config.managers = vec![Manager::Gitlabci, Manager::GitlabciInclude];
    let scanner = Scanner::new(&config).unwrap();
    assert!(scanner.needs(".gitlab-ci.yml"));
    assert!(scanner.needs("ci/deploy.gitlab-ci.yaml"));
    assert!(!scanner.needs("ci/templates.yml"));
    let text = r#"spec:
  inputs:
    stage:
      default: test
---
include:
  - local: ci/lint.yml
  - project: my-group/my-project
    ref: v1.2.0
    file: /templates/build.yml
  - project: 'grp/sub/proj'
    ref: "2.0.1"
    file: [a.yml, b.yml]
  - project: grp/pinned
    ref: 0123456789abcdef0123456789abcdef01234567
  - project: grp/default-branch
    file: ci.yml
  - project: $TEMPLATES
    ref: v1
  - component: $CI_SERVER_FQDN/components/sast/sast@3.4.0
  - component: "gitlab.example.com/grp/tools/lint@1.0.0"
  - component: gitlab.com/components/opentofu/full-pipeline@~latest
  - component: $OTHER_HOST/grp/proj/name@1.0.0
  - remote: https://example.com/ci.yml
image: ruby:3.3
variables:
  image: not-an-image:1
default:
  services:
    - postgres:16.3
    - name: redis:7.2-alpine
      alias: cache
.base: &base
  image:
    name: node:22-slim
    entrypoint: [""]
build:
  <<: *base
  script: [make]
test/unit:
  image: $CI_REGISTRY_IMAGE:latest
  services:
    - name: ${CI_DEPENDENCY_PROXY_GROUP_IMAGE_PREFIX}/mysql:8
pinned:
  image: "alpine:3.20@sha256:0000000000000000000000000000000000000000000000000000000000000000"
"#;
    let files = Files::from([(".gitlab-ci.yml".into(), text.into())]);
    let extraction = scanner.extract(&files).unwrap();
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);
    let mut got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            (
                d.manager,
                d.kind,
                d.source,
                d.package.as_str(),
                d.current.as_str(),
                d.digest.is_some(),
            )
        })
        .collect();
    got.sort_by(|a, b| a.3.cmp(b.3));
    use DependencyKind::{ContainerImage as Image, ReusableWorkflow as Include};
    use Manager::{Gitlabci as Ci, GitlabciInclude as Project};
    assert_eq!(
        got,
        [
            (Ci, Image, Source::Docker, "alpine", "3.20", true),
            (
                Ci,
                Include,
                Source::ForgeTags,
                "gitlab.com/components/sast",
                "3.4.0",
                false
            ),
            (
                Project,
                Include,
                Source::ForgeTags,
                "gitlab.com/grp/sub/proj",
                "2.0.1",
                false
            ),
            (
                Project,
                Include,
                Source::ForgeTags,
                "gitlab.com/my-group/my-project",
                "v1.2.0",
                false
            ),
            (
                Ci,
                Include,
                Source::ForgeTags,
                "gitlab.example.com/grp/tools",
                "1.0.0",
                false
            ),
            (Ci, Image, Source::Docker, "node", "22-slim", false),
            (Ci, Image, Source::Docker, "postgres", "16.3", false),
            (Ci, Image, Source::Docker, "redis", "7.2-alpine", false),
            (Ci, Image, Source::Docker, "ruby", "3.3", false),
        ]
    );
    for dep in &extraction.dependencies {
        let slice = text[dep.location.start..dep.location.end].trim_matches(['"', '\'']);
        if dep.kind == DependencyKind::ContainerImage {
            assert!(
                slice.starts_with(&format!("{}:{}", dep.package, dep.current)),
                "{slice}"
            );
        } else {
            assert_eq!(slice, dep.current);
        }
    }
    let sast = extraction
        .dependencies
        .iter()
        .find(|d| d.current == "3.4.0")
        .unwrap();
    assert_eq!(sast.name, "gitlab.com/components/sast/sast");
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.manager, s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                Ci,
                Some(23),
                "$OTHER_HOST/grp/proj/name@1.0.0",
                "the component's host is a GitLab CI variable other than $CI_SERVER_FQDN"
            ),
            (
                Ci,
                Some(41),
                "$CI_REGISTRY_IMAGE:latest",
                "the image contains a GitLab CI variable"
            ),
            (
                Ci,
                Some(43),
                "${CI_DEPENDENCY_PROXY_GROUP_IMAGE_PREFIX}/mysql:8",
                "the image contains a GitLab CI variable"
            ),
            (
                Project,
                Some(19),
                "$TEMPLATES@v1",
                "the include contains a GitLab CI variable"
            ),
        ]
    );
}
#[test]
fn gitlab_include_projects_resolve_on_the_configured_host() {
    let mut config = config();
    config.managers = vec![Manager::Gitlabci, Manager::GitlabciInclude];
    config.gitlab.host = "gitlab.example.com".into();
    let scanner = Scanner::new(&config).unwrap();
    let text = "include:\n  project: grp/proj\n  ref: 1.0\n---\ninclude:\n  - component: $CI_SERVER_HOST/grp/comp/name@2.1.0\n";
    let files = Files::from([("sub/.gitlab-ci.yaml".into(), text.into())]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let got: Vec<_> = deps
        .iter()
        .map(|d| {
            (
                d.package.as_str(),
                d.current.as_str(),
                d.location.pointer.as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "gitlab.example.com/grp/comp",
                "2.1.0",
                "/1/include/0/component"
            ),
            ("gitlab.example.com/grp/proj", "1.0", "/0/include/ref"),
        ]
    );
}
#[test]
fn gitlab_ci_aliased_images_build_metadata_and_numeric_refs() {
    let mut config = config();
    config.managers = vec![Manager::Gitlabci, Manager::GitlabciInclude];
    let scanner = Scanner::new(&config).unwrap();
    let text = "variables:\n  BASE: &base alpine:3.20\ninclude:\n  - component: gitlab.com/grp/proj/comp@1.2.3+build.1\n  - project: grp/proj\n    ref: 1.0\none:\n  image: *base\ntwo:\n  image: *base\n";
    let files = Files::from([(".gitlab-ci.yml".into(), text.into())]);
    let deps = scanner.extract(&files).unwrap().dependencies;
    let mut got: Vec<_> = deps
        .iter()
        .map(|d| (d.package.as_str(), d.current.as_str(), d.location.line))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("alpine", "3.20", 2),
            ("gitlab.com/grp/proj", "1.0", 6),
            ("gitlab.com/grp/proj", "1.2.3+build.1", 4),
        ]
    );
}
#[test]
fn gitlab_refs_shared_through_an_anchor_are_skipped() {
    let mut config = config();
    config.managers = vec![Manager::GitlabciInclude];
    let scanner = Scanner::new(&config).unwrap();
    let text = "variables:\n  V: &v 1.0.0\ninclude:\n  - project: grp/a\n    ref: *v\n  - project: grp/b\n    ref: *v\n  - project: grp/c\n    ref: 2.0.0\n";
    let files = Files::from([(".gitlab-ci.yml".into(), text.into())]);
    let extraction = scanner.extract(&files).unwrap();
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| d.package.as_str())
        .collect();
    assert_eq!(got, ["gitlab.com/grp/c"]);
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str()))
        .collect();
    assert_eq!(skipped, [(Some(2), "grp/a@1.0.0")]);
}

#[test]
fn copier_answers_read_template_and_tag_and_skip_what_cannot_be_looked_up() {
    let mut config = config();
    config.managers = vec![Manager::Copier];
    let scanner = Scanner::new(&config).unwrap();
    for (path, needed) in [
        (".copier-answers.yml", true),
        (".copier-answers.yaml", true),
        ("backend/.copier-answers.yml", true),
        (".copier-answers.ci.yml", true),
        (".copier-answers.docs.site.yaml", true),
        (".copier-answers..yml", false),
        ("copier-answers.yml", false),
        (".copier-answers.json", false),
        ("copier.yml", false),
    ] {
        assert_eq!(scanner.needs(path), needed, "{path}");
    }
    let answers = |src: &str, commit: &str| {
        format!(
            "# Changes here will be overwritten by Copier\n_commit: {commit}\n_src_path: {src}\nproject_name: demo\n"
        )
    };
    let files = Files::from([
        (
            ".copier-answers.yml".into(),
            answers("gh:acme/python-template", "v1.4.0"),
        ),
        (
            "api/.copier-answers.yml".into(),
            answers("git@gitlab.com:group/sub/template.git", "2.0.1"),
        ),
        (
            "web/.copier-answers.web.yaml".into(),
            answers("https://codeberg.org/acme/web-template", "v0.3.0"),
        ),
        (
            "docs/.copier-answers.yml".into(),
            answers("gl:acme/docs-template", "'1.2'"),
        ),
        (
            "ssh/.copier-answers.yml".into(),
            answers("git+ssh://git@github.com/acme/ssh-template.git", "v3.0.0"),
        ),
        (
            "elsewhere/.copier-answers.yml".into(),
            answers("https://git.example.com/acme/template.git", "v1.0.0"),
        ),
        (
            "local/.copier-answers.yml".into(),
            answers("../templates/local", "v1.0.0"),
        ),
        (
            "untagged/.copier-answers.yml".into(),
            answers("gh:acme/untagged", "v1.4.0-5-g1a2b3c4"),
        ),
        (
            "nocommit/.copier-answers.yml".into(),
            "_src_path: gh:acme/nocommit\n".into(),
        ),
    ]);
    let extraction = scanner.extract(&files).unwrap();
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            assert_eq!(d.kind, DependencyKind::Template);
            assert_eq!(d.scheme, patchula_config::Scheme::Pep440);
            (
                d.file.as_str(),
                d.name.as_str(),
                d.package.as_str(),
                patchula_manager::word(&d.source),
                d.current.as_str(),
                &files[&d.file][d.location.start..d.location.end],
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                ".copier-answers.yml",
                "gh:acme/python-template",
                "acme/python-template",
                "github-tags".into(),
                "v1.4.0",
                "v1.4.0"
            ),
            (
                "api/.copier-answers.yml",
                "git@gitlab.com:group/sub/template.git",
                "gitlab.com/group/sub/template",
                "forge-tags".into(),
                "2.0.1",
                "2.0.1"
            ),
            (
                "docs/.copier-answers.yml",
                "gl:acme/docs-template",
                "gitlab.com/acme/docs-template",
                "forge-tags".into(),
                "1.2",
                "'1.2'"
            ),
            (
                "ssh/.copier-answers.yml",
                "git+ssh://git@github.com/acme/ssh-template.git",
                "acme/ssh-template",
                "github-tags".into(),
                "v3.0.0",
                "v3.0.0"
            ),
            (
                "web/.copier-answers.web.yaml",
                "https://codeberg.org/acme/web-template",
                "codeberg.org/acme/web-template",
                "forge-tags".into(),
                "v0.3.0",
                "v0.3.0"
            ),
        ]
    );
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.file.as_str(), s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                "elsewhere/.copier-answers.yml",
                Some(3),
                "https://git.example.com/acme/template.git",
                "no tag source for git.example.com/acme/template: the Count looks up templates in GitHub, GitLab, Gitea and Forgejo repositories"
            ),
            (
                "local/.copier-answers.yml",
                Some(3),
                "../templates/local",
                "a local template has no tags the Count can look up"
            ),
            (
                "nocommit/.copier-answers.yml",
                Some(1),
                "gh:acme/nocommit",
                "the answers file records no `_commit`"
            ),
            (
                "untagged/.copier-answers.yml",
                Some(3),
                "gh:acme/untagged",
                "`_commit` v1.4.0-5-g1a2b3c4 is not a release tag, so there is nothing to compare newer tags with"
            ),
        ]
    );
}

#[test]
fn flake_inputs_come_from_the_lock_beside_flake_nix() {
    let lock = r#"{
  "nodes": {
    "nixpkgs": {
      "locked": {"lastModified": 1790000000, "narHash": "sha256-a", "owner": "NixOS", "repo": "nixpkgs", "rev": "1111111111111111111111111111111111111111", "type": "github"},
      "original": {"owner": "NixOS", "ref": "nixos-unstable", "repo": "nixpkgs", "type": "github"}
    },
    "utils": {
      "locked": {"lastModified": 1, "narHash": "sha256-b", "owner": "numtide", "repo": "flake-utils", "rev": "2222222222222222222222222222222222222222", "type": "github"},
      "original": {"owner": "numtide", "ref": "v1.0.0", "repo": "flake-utils", "type": "github"}
    },
    "registry": {
      "locked": {"lastModified": 2, "narHash": "sha256-c", "owner": "NixOS", "repo": "nixpkgs", "rev": "3333333333333333333333333333333333333333", "type": "github"},
      "original": {"id": "nixpkgs", "type": "indirect"}
    },
    "lib": {
      "locked": {"lastModified": 3, "narHash": "sha256-d", "ref": "refs/heads/main", "rev": "4444444444444444444444444444444444444444", "type": "git", "url": "https://example.com/lib.git"},
      "original": {"ref": "main", "type": "git", "url": "https://example.com/lib.git"}
    },
    "archive": {
      "locked": {"narHash": "sha256-e", "type": "tarball", "url": "https://example.com/a.tar.gz"},
      "original": {"type": "tarball", "url": "https://example.com/a.tar.gz"}
    },
    "local": {
      "locked": {"path": "./sub", "type": "path"},
      "original": {"path": "./sub", "type": "path"},
      "parent": []
    },
    "escape": {
      "locked": {"lastModified": 4, "narHash": "sha256-f", "path": "/etc", "type": "path"},
      "original": {"path": "/etc", "type": "path"}
    },
    "private": {
      "locked": {"lastModified": 5, "narHash": "sha256-g", "rev": "5555555555555555555555555555555555555555", "type": "git", "url": "ssh://git@example.com/private.git"},
      "original": {"type": "git", "url": "ssh://git@example.com/private.git"}
    },
    "root": {
      "inputs": {"archive": "archive", "escape": "escape", "lib": "lib", "local": "local", "nixpkgs": "nixpkgs", "private": "private", "registry": "registry", "same": ["nixpkgs"], "utils": "utils"}
    }
  },
  "root": "root",
  "version": 7
}
"#;
    let mut config = config();
    config.managers.push(Manager::Nix);
    let scanner = Scanner::new(&config).unwrap();
    assert!(scanner.needs("dev/flake.lock") && scanner.needs("dev/flake.nix"));
    // A lock without its flake.nix is not a flake.
    let alone = scanner
        .extract(&Files::from([("dev/flake.lock".into(), lock.into())]))
        .unwrap();
    assert!(alone.dependencies.is_empty());
    let files = Files::from([
        ("dev/flake.lock".into(), lock.to_string()),
        (
            "dev/flake.nix".into(),
            "{ outputs = _: { }; }\n".to_string(),
        ),
    ]);
    let extraction = scanner.extract(&files).unwrap();
    let found: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            assert_eq!(d.manager, Manager::Nix);
            assert_eq!(d.kind, DependencyKind::FlakeInput);
            assert_eq!(
                &files[&d.file][d.location.start..d.location.end],
                d.digest.as_deref().unwrap()
            );
            (d.name.as_str(), d.package.as_str(), d.current.as_str())
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "archive",
                "https://example.com/a.tar.gz",
                "tarball+https://example.com/a.tar.gz"
            ),
            (
                "lib",
                "https://example.com/lib.git",
                "git+https://example.com/lib.git?ref=main"
            ),
            (
                "nixpkgs",
                "https://github.com/NixOS/nixpkgs",
                "github:NixOS/nixpkgs/nixos-unstable"
            ),
            ("registry", "flake:nixpkgs", "flake:nixpkgs"),
            (
                "utils",
                "https://github.com/numtide/flake-utils",
                "github:numtide/flake-utils/v1.0.0"
            ),
        ]
    );
    let archive = &extraction.dependencies[0];
    assert_eq!(archive.digest.as_deref(), Some("sha256-e"));
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("path:/etc", "it resolves outside the repository"),
            ("path:./sub", "a path input changes with the repository"),
            (
                "git+ssh://git@example.com/private.git",
                "ssh is not allowed; only https"
            ),
        ]
    );
    assert!(extraction.skipped.iter().all(|s| s.line.is_some()));
}

#[test]
fn apm_git_tags_commit_pins_and_skipped_entries() {
    let mut config = config();
    config.managers = vec![Manager::Apm];
    let scanner = Scanner::new(&config).unwrap();
    for (path, needed) in [
        ("apm.yml", true),
        ("apm.yaml", true),
        ("packages/skill/apm.yml", true),
        ("apm.lock.yaml", false),
        ("myapm.yml", false),
    ] {
        assert_eq!(scanner.needs(path), needed, "{path}");
    }
    let text = "name: demo\nversion: 1.0.0\ndependencies:\n  apm:\n    - microsoft/apm-sample-package#v1.0.0  # Design standards\n    - gitlab.com/team/project#v2.3.0\n    - owner/repo/skills/review#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c # v1.2.3 keep\n    - 'codeberg.org/forge/pack#1.4'\n    - owner/unpinned\n    - owner/branch#main\n    - owner/short#abc1234\n    - owner/bare#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c\n    - bitbucket.org/team/repo#v1.0.0\n    - https://github.com/owner/url.git\n    - ./packages/local\n    - git: https://gitlab.com/acme/repo.git\n      ref: v2.0\n    - path: ./local\n  mcp:\n    - owner/mcp#v1.0.0\ndevDependencies:\n  apm:\n    - owner/dev-tools#v0.3.0\n".to_string();
    let files = Files::from([("apm.yml".into(), text.clone())]);
    let extraction = scanner.extract(&files).unwrap();
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            (
                d.name.as_str(),
                d.package.as_str(),
                d.source,
                d.kind,
                d.current.as_str(),
                d.digest.is_some(),
                &text[d.location.start..d.location.end],
            )
        })
        .collect();
    use patchula_config::Source;
    assert_eq!(
        got,
        [
            (
                "microsoft/apm-sample-package",
                "microsoft/apm-sample-package",
                Source::GithubTags,
                DependencyKind::Production,
                "v1.0.0",
                false,
                "microsoft/apm-sample-package#v1.0.0  # Design standards"
            ),
            (
                "gitlab.com/team/project",
                "gitlab.com/team/project",
                Source::ForgeTags,
                DependencyKind::Production,
                "v2.3.0",
                false,
                "gitlab.com/team/project#v2.3.0"
            ),
            (
                "owner/repo/skills/review",
                "owner/repo",
                Source::GithubTags,
                DependencyKind::Production,
                "v1.2.3",
                true,
                "owner/repo/skills/review#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c # v1.2.3 keep"
            ),
            (
                "codeberg.org/forge/pack",
                "codeberg.org/forge/pack",
                Source::ForgeTags,
                DependencyKind::Production,
                "1.4",
                false,
                "'codeberg.org/forge/pack#1.4'"
            ),
            (
                "owner/dev-tools",
                "owner/dev-tools",
                Source::GithubTags,
                DependencyKind::Development,
                "v0.3.0",
                false,
                "owner/dev-tools#v0.3.0"
            ),
        ]
    );
    assert!(
        extraction
            .dependencies
            .iter()
            .all(|d| d.manager == Manager::Apm)
    );
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (Some(9), "owner/unpinned"),
            (Some(10), "owner/branch#main"),
            (Some(11), "owner/short#abc1234"),
            (
                Some(12),
                "owner/bare#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c"
            ),
            (Some(13), "bitbucket.org/team/repo#v1.0.0"),
            (Some(14), "https://github.com/owner/url.git"),
            (Some(16), "https://gitlab.com/acme/repo.git"),
        ]
    );
    assert!(extraction.skipped.iter().all(|s| !s.reason.is_empty()));
}

#[test]
fn apm_tab_separated_comments_and_flow_lists_are_left_alone() {
    let mut config = config();
    config.managers = vec![Manager::Apm];
    let text = "dependencies:\n  apm: [owner/flow#v1.0.0]\ndevDependencies:\n  apm:\n    - owner/tab#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c\t# v1.0.0\n    - owner/tagged#v1.0.0\t# note\n";
    let files = Files::from([("apm.yml".into(), text.to_string())]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| &text[d.location.start..d.location.end])
        .collect();
    assert_eq!(got, ["owner/tagged#v1.0.0"]);
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.line, s.value.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (Some(2), "owner/flow#v1.0.0"),
            (
                Some(5),
                "owner/tab#fb2851683d0b49ef9e1e0c6d0a9a3e6a1f0e2b3c"
            )
        ]
    );
}

#[test]
fn apm_block_entries_with_anchors_or_on_their_own_line_are_read() {
    let mut config = config();
    config.managers = vec![Manager::Apm];
    let text = "dependencies:\n  apm:\n    - &pkg owner/anchored#v1.0.0\n    -\n      owner/next-line#v2.0.0\n    - !!str owner/tagged#v3.0.0\n";
    let files = Files::from([("apm.yml".into(), text.to_string())]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let got: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(got, ["owner/anchored", "owner/next-line", "owner/tagged"]);
    assert!(extraction.skipped.is_empty(), "{:?}", extraction.skipped);
}

#[test]
fn cargo_local_and_alternate_registry_dependencies_are_skipped() {
    let mut config = config();
    config.managers = vec![Manager::Cargo];
    let files = Files::from([
        (
            "Cargo.toml".into(),
            r#"[workspace]
members = ["member"]

[workspace.dependencies]
shared = { version = "0.6.0", path = "shared" }
plain = "1.0.0"

[dependencies]
local = { version = "0.6.0", path = "../local" }
forked = { git = "https://github.com/acme/forked", version = "2.0.0" }
private = { version = "3.0.0", registry = "acme" }
path-only = { path = "../path-only" }
git-only = { git = "https://github.com/acme/git-only" }
serde = "1.0.0"
explicit = { version = "4.0.0", registry = "crates-io" }
"#
            .into(),
        ),
        (
            "member/Cargo.toml".into(),
            "[dependencies]\nshared = { workspace = true }\nplain = { workspace = true }\n".into(),
        ),
    ]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let emitted: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str(), d.current.as_str()))
        .collect();
    assert_eq!(
        emitted,
        [
            ("Cargo.toml", "plain", "1.0.0"),
            ("Cargo.toml", "serde", "1.0.0"),
            ("Cargo.toml", "explicit", "4.0.0"),
            ("member/Cargo.toml", "plain", "1.0.0"),
        ]
    );
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.file.as_str(), s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                "Cargo.toml",
                Some(5),
                "shared",
                "a path dependency, not looked up on crates.io"
            ),
            (
                "Cargo.toml",
                Some(9),
                "local",
                "a path dependency, not looked up on crates.io"
            ),
            (
                "Cargo.toml",
                Some(10),
                "forked",
                "a git dependency, not looked up on crates.io"
            ),
            (
                "Cargo.toml",
                Some(11),
                "private",
                "from the alternate registry `acme`, which the Count does not look up"
            ),
            (
                "member/Cargo.toml",
                Some(2),
                "shared",
                "a path dependency, not looked up on crates.io"
            ),
        ]
    );
}

#[test]
fn mise_http_and_s3_tools_are_skipped_because_they_list_no_versions() {
    let mut config = config();
    config.managers = vec![Manager::Mise];
    let files = Files::from([
        (
            "mise.toml".into(),
            "[tools]\njq = \"1.7.1\"\n\"http:deepsource\" = { version = \"2.0.55\", url = \"https://example.com/{{ version }}.tar.gz\" }\n\"s3:internal\" = \"1.0.0\"\n\"github:org/tool\" = { version = \"v2.0.0\" }\n".into(),
        ),
        (".tool-versions".into(), "jq 1.7.1\nhttp:other 3.0.0\n".into()),
    ]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let emitted: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| (d.file.as_str(), d.name.as_str()))
        .collect();
    assert_eq!(
        emitted,
        [
            (".tool-versions", "jq"),
            ("mise.toml", "jq"),
            ("mise.toml", "github:org/tool"),
        ]
    );
    let skipped: Vec<_> = extraction
        .skipped
        .iter()
        .map(|s| (s.file.as_str(), s.line, s.value.as_str(), s.reason.as_str()))
        .collect();
    let http = "mise's http backend lists versions only with version_list_url, which the Count does not pass";
    let s3 = "mise's s3 backend lists versions only with version_list_url or version_prefix, which the Count does not pass";
    assert_eq!(
        skipped,
        [
            (".tool-versions", Some(2), "http:other", http),
            ("mise.toml", Some(3), "http:deepsource", http),
            ("mise.toml", Some(4), "s3:internal", s3),
        ]
    );
}

/// mise installs a task's `tools` for that task only, in a config file under
/// `[tasks.<name>]` and in an included task file as a top-level `[<name>]`.
#[test]
fn mise_task_tools_are_extracted_from_config_and_included_task_files() {
    let mut config = config();
    config.managers = vec![Manager::Mise];
    config.files = vec![patchula_config::FileSet {
        manager: Manager::Mise,
        paths: vec!["mise.toml".into(), "tasks.toml".into()],
    }];
    let files = Files::from([
        (
            "mise.toml".into(),
            "[task_config]\nincludes = [\"tasks.toml\"]\n[tools]\njq = \"1.7.1\"\n[tasks.lint]\nrun = \"hk check\"\ntools.hk = \"1.2.0\"\n[tasks.test]\ntools = { python = \"3.14.8\", uv = { version = \"0.13.1\" } }\n[tasks.plain]\nrun = \"true\"\n[tasks]\nshort = \"echo hi\"\n".into(),
        ),
        (
            "tasks.toml".into(),
            "short = \"echo hi\"\n[build]\nrun = \"cargo build\"\n[\"test:tools\".tools]\naube = \"2.8.1\"\n\"http:php[url=https://example.com/{{version}}.tar.gz]\" = \"8.5.8\"\n".into(),
        ),
    ]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let found: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            (
                d.file.as_str(),
                d.name.as_str(),
                d.current.as_str(),
                d.location.pointer.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("mise.toml", "jq", "1.7.1", "/tools/jq"),
            ("mise.toml", "hk", "1.2.0", "/tasks/lint/tools/hk"),
            ("mise.toml", "python", "3.14.8", "/tasks/test/tools/python"),
            ("mise.toml", "uv", "0.13.1", "/tasks/test/tools/uv"),
            ("tasks.toml", "aube", "2.8.1", "/test:tools/tools/aube"),
        ]
    );
    for dep in &extraction.dependencies {
        assert_eq!(
            files[&dep.file][dep.location.start..dep.location.end].trim_matches('"'),
            dep.current
        );
    }
    assert_eq!(
        extraction
            .skipped
            .iter()
            .map(|s| (s.file.as_str(), s.line))
            .collect::<Vec<_>>(),
        [("tasks.toml", Some(6))]
    );
}

fn mise_files(paths: &[&str]) -> patchula_config::Config {
    let mut config = config();
    config.managers = vec![Manager::Mise];
    config.files = vec![patchula_config::FileSet {
        manager: Manager::Mise,
        paths: paths.iter().map(|p| (*p).into()).collect(),
    }];
    config
}

/// In a task file every top-level table is a task, whatever its name; a file no
/// config includes is read as a config.
#[test]
fn mise_task_files_are_found_by_what_includes_them_not_by_their_names() {
    let config = mise_files(&["mise.toml", "tasks/**/*.toml", "scripts/*.toml"]);
    let files = Files::from([
        (
            "mise.toml".into(),
            "[task_config]\nincludes = [\"tasks/*.toml\", \"scripts\"]\n".into(),
        ),
        (
            "tasks/ci.toml".into(),
            "[tools]\nrun = \"echo\"\ntools = { jq = \"1.7.1\" }\n[lint]\ntools = { bun = \"1.4.3\" }\n".into(),
        ),
        (
            "scripts/more.toml".into(),
            "[tasks]\ntools = { go = \"1.27.2\" }\n".into(),
        ),
        // The glob stops at one level, so this file is nobody's task file.
        ("tasks/deep/own.toml".into(), "[tools]\nnode = \"26.0.0\"\n".into()),
    ]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let found: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| {
            (
                d.file.as_str(),
                d.name.as_str(),
                d.location.pointer.as_str(),
                d.included_by.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "scripts/more.toml",
                "go",
                "/tasks/tools/go",
                Some("mise.toml")
            ),
            ("tasks/ci.toml", "jq", "/tools/tools/jq", Some("mise.toml")),
            ("tasks/ci.toml", "bun", "/lint/tools/bun", Some("mise.toml")),
            ("tasks/deep/own.toml", "node", "/tools/node", None),
        ]
    );
}

/// An include reaches a file, a glob's matches or a directory's TOML files at
/// any depth, from the including file's directory, and never leaves the
/// repository.
#[test]
fn mise_includes_resolve_from_the_including_directory_inside_the_repository() {
    let paths = [
        "tasks.toml",
        "sub/tasks.toml",
        "sub/deep/more.toml",
        "sub/notes.md",
        "other/a.toml",
        "other/nested/b.toml",
    ];
    let found = |directory: &str, include: &str| {
        patchula_manager::included_files(directory, include, paths.iter().copied())
    };
    assert_eq!(found("", "tasks.toml"), ["tasks.toml"]);
    assert_eq!(found("", "./sub/../tasks.toml"), ["tasks.toml"]);
    assert_eq!(found("sub", "../tasks.toml"), ["tasks.toml"]);
    assert_eq!(found("", "sub"), ["sub/tasks.toml", "sub/deep/more.toml"]);
    assert_eq!(found("", "sub/*.toml"), ["sub/tasks.toml"]);
    assert_eq!(
        found("", "other/**/*.toml"),
        ["other/a.toml", "other/nested/b.toml"]
    );
    assert!(found("", "../tasks.toml").is_empty());
    assert!(found("sub", "../../tasks.toml").is_empty());
    assert!(found("", "/tasks.toml").is_empty());
}

/// Task and tool names are JSON Pointer tokens, so `/` and `~` are escaped.
#[test]
fn mise_pointers_escape_slashes_and_tildes_in_names() {
    let config = mise_files(&["mise.toml"]);
    let files = Files::from([(
        "mise.toml".into(),
        "[tools]\n\"github:org/tool\" = \"1.0.0\"\n[tasks.\"a/b~c\"]\ntools = { \"github:org/other\" = \"2.0.0\" }\n".into(),
    )]);
    let extraction = Scanner::new(&config).unwrap().extract(&files).unwrap();
    let pointers: Vec<_> = extraction
        .dependencies
        .iter()
        .map(|d| d.location.pointer.as_str())
        .collect();
    assert_eq!(
        pointers,
        [
            "/tools/github:org~1tool",
            "/tasks/a~1b~0c/tools/github:org~1other"
        ]
    );
}

/// `.` and `./` include the repository root like any directory, at any depth,
/// except what mise reads as configuration: `mise.toml`, `mise.local.toml` and
/// hidden files.
#[test]
fn mise_includes_of_the_repository_root_reach_every_task_file_below_it() {
    let paths = [
        "mise.toml",
        ".mise.toml",
        "mise.local.toml",
        "tasks.toml",
        "sub/mise.toml",
        "sub/deep/more.toml",
        "sub/.hidden.toml",
        "README.md",
    ];
    let found = |directory: &str, include: &str| {
        patchula_manager::included_files(directory, include, paths.iter().copied())
    };
    let everything = ["tasks.toml", "sub/deep/more.toml"];
    assert_eq!(found("", "."), everything);
    assert_eq!(found("", "./"), everything);
    assert_eq!(found("sub", ".."), everything);
    assert_eq!(found("sub", "./deep"), ["sub/deep/more.toml"]);
    // Naming a file asks for that file, whatever it is called.
    assert_eq!(found("", "mise.local.toml"), ["mise.local.toml"]);
}
