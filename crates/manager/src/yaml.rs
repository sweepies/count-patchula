use crate::*;
use serde_yaml_ng::Value;

fn scalar(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_i64().map(|v| v.to_string()))
        .or_else(|| v.as_f64().map(|v| v.to_string()))
}
fn get(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(scalar)
}
pub(super) fn docs(text: &str) -> Result<Vec<Value>, String> {
    serde_yaml_ng::Deserializer::from_str(text)
        .map(|doc| Value::deserialize(doc).map_err(|e| e.to_string()))
        .collect()
}

/// Flux references use namespace/name, not globally ambiguous repository names.
pub(super) fn repositories(
    files: &BTreeMap<&str, Vec<Value>>,
) -> BTreeMap<(String, String), (String, bool)> {
    let mut out = BTreeMap::new();
    for docs in files.values() {
        for doc in docs {
            if get(doc, "kind").as_deref() == Some("HelmRepository")
                && let Some(spec) = doc.get("spec")
                && let Some(url) = get(spec, "url")
            {
                let metadata = &doc["metadata"];
                if let Some(name) = get(metadata, "name") {
                    out.insert(
                        (get(metadata, "namespace").unwrap_or_default(), name),
                        (url, get(spec, "type").as_deref() == Some("oci")),
                    );
                }
            }
        }
    }
    out
}
pub(super) struct Scan<'a> {
    path: &'a str,
    text: &'a str,
    manager: Manager,
    locations: &'a BTreeMap<String, Location>,
    out: Vec<Dependency>,
}
impl<'a> Scan<'a> {
    fn location(&self, _key: &str, _value: &str, pointer: &str) -> Result<Location, String> {
        self.locations.get(pointer).cloned().ok_or_else(||format!("cannot locate YAML scalar {pointer}; aliased/template scalars need a concrete value"))
    }
    fn image(&mut self, image: &str, key: &str, value: &str, pointer: &str) -> Result<(), String> {
        if image.contains(['$', '{']) {
            return Ok(());
        }
        let location = self.location(key, value, pointer)?;
        if let Some(dep) = images::dependency(self.manager, self.path, self.text, image, location) {
            self.out.push(dep);
        }
        Ok(())
    }
    fn chart(
        &mut self,
        name: &str,
        package: String,
        source: Source,
        version: &Value,
        pointer: &str,
    ) -> Result<(), String> {
        let Some(current) = scalar(version) else {
            return Ok(());
        };
        let key = pointer.rsplit('/').next().unwrap();
        let location = self.location(key, &current, pointer)?;
        self.out.push(Dependency {
            manager: self.manager,
            file: self.path.into(),
            location,
            name: name.into(),
            package,
            kind: DependencyKind::Chart,
            current,
            resolved: None,
            digest: None,
            source,
            scheme: Scheme::Semver,
            extract: None,
        });
        Ok(())
    }
    fn images(&mut self, value: &Value, pointer: &str, values: bool) -> Result<(), String> {
        match value {
            Value::Mapping(map) => {
                if values
                    && let (Some(repo), Some(tag)) =
                        (get(value, "repository"), value.get("tag").and_then(scalar))
                {
                    let repo = get(value, "registry")
                        .filter(|r| !r.is_empty())
                        .map_or(repo.clone(), |r| format!("{r}/{repo}"));
                    let image = format!(
                        "{repo}:{tag}{}",
                        get(value, "digest")
                            .filter(|d| !d.is_empty())
                            .map_or(String::new(), |d| format!("@{d}"))
                    );
                    self.image(&image, "tag", &tag, &format!("{pointer}/tag"))?;
                }
                for (key, v) in map {
                    let Some(key) = key.as_str() else {
                        continue;
                    };
                    let p = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                    if key == "image"
                        && let Some(image) = v.as_str()
                    {
                        self.image(image, "image", image, &p)?;
                    } else {
                        self.images(v, &p, values)?;
                    }
                }
            }
            Value::Sequence(items) => {
                for (i, v) in items.iter().enumerate() {
                    self.images(v, &format!("{pointer}/{i}"), values)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
pub(super) fn extract(
    manager: Manager,
    path: &str,
    text: &str,
    documents: &[Value],
    locations: &BTreeMap<String, Location>,
    repos: &BTreeMap<(String, String), (String, bool)>,
) -> Result<Vec<Dependency>, String> {
    let mut scan = Scan {
        path,
        text,
        manager,
        locations,
        out: Vec::new(),
    };
    if manager == Manager::Flux
        && text
            .lines()
            .any(|line| line == "# This manifest was generated by flux. DO NOT EDIT.")
    {
        let pattern = regex::Regex::new(r"(?m)^# Flux Version: (?<version>v\d+\.\d+\.\d+)[ \t]*$")
            .map_err(|e| e.to_string())?;
        if let Some(version) = pattern.captures(text).and_then(|c| c.name("version")) {
            // The version identifies a generated bundle. A future writer must
            // regenerate its resources, rather than editing only this comment.
            scan.out.push(Dependency {
                manager,
                file: path.into(),
                location: Location::new(
                    text,
                    version.start(),
                    version.end(),
                    "/flux-version".into(),
                ),
                name: "fluxcd/flux2".into(),
                package: "fluxcd/flux2".into(),
                kind: DependencyKind::Toolchain,
                current: version.as_str().into(),
                resolved: Some(version.as_str().into()),
                digest: None,
                source: Source::GithubReleases,
                scheme: Scheme::Loose,
                extract: None,
            });
        }
    }
    for (index, doc) in documents.iter().enumerate() {
        let pointer = format!("/{index}");
        let kind = get(doc, "kind").unwrap_or_default();
        match manager {
            Manager::Compose => {
                if let Some(services) = doc.get("services").and_then(Value::as_mapping) {
                    for (name, service) in services {
                        if let Some(image) = get(service, "image") {
                            scan.image(
                                &image,
                                "image",
                                &image,
                                &format!(
                                    "{pointer}/services/{}/image",
                                    name.as_str().unwrap_or("?")
                                ),
                            )?;
                        }
                    }
                }
            }
            Manager::Kubernetes => {
                if !kind.is_empty()
                    && !matches!(
                        kind.as_str(),
                        "ConfigMap"
                            | "Secret"
                            | "HelmRelease"
                            | "HelmRepository"
                            | "OCIRepository"
                            | "Kustomization"
                    )
                {
                    if let Some(spec) = doc.get("spec") {
                        scan.images(spec, &format!("{pointer}/spec"), false)?;
                    }
                    if let Some(items) = doc.get("items") {
                        scan.images(items, &format!("{pointer}/items"), false)?;
                    }
                }
            }
            Manager::HelmValues => {
                if kind.is_empty() && doc.get("services").is_none() && doc.get("jobs").is_none() {
                    scan.images(doc, &pointer, true)?;
                }
                // Environment-backed controller images are concrete config
                // values, even though they are not pod container nodes.
                if kind == "ConfigMap"
                    && let Some(data) = doc.get("data").and_then(Value::as_mapping)
                {
                    for (key, value) in data {
                        if let (Some(key), Some(image)) = (key.as_str(), value.as_str())
                            && key.ends_with("_IMAGE")
                            && image.contains([':', '@'])
                        {
                            scan.image(image, key, image, &format!("{pointer}/data/{key}"))?;
                        }
                    }
                }
            }
            Manager::Kustomize => {
                if let Some(items) = doc.get("images").and_then(Value::as_sequence) {
                    for (i, image) in items.iter().enumerate() {
                        let name = get(image, "newName").or_else(|| get(image, "name"));
                        if let (Some(name), Some(tag)) = (name, get(image, "newTag")) {
                            let full = format!(
                                "{name}:{tag}{}",
                                get(image, "digest").map_or(String::new(), |d| format!("@{d}"))
                            );
                            scan.image(
                                &full,
                                "newTag",
                                &tag,
                                &format!("{pointer}/images/{i}/newTag"),
                            )?;
                        } else if let Some(image) = image.as_str() {
                            scan.image(image, "images", image, &format!("{pointer}/images/{i}"))?;
                        }
                    }
                }
                if let Some(charts) = doc.get("helmCharts").and_then(Value::as_sequence) {
                    for (i, chart) in charts.iter().enumerate() {
                        if let (Some(name), Some(repo), Some(version)) =
                            (get(chart, "name"), get(chart, "repo"), chart.get("version"))
                        {
                            scan.chart(
                                &name,
                                if repo.starts_with("oci://") {
                                    format!(
                                        "{}/{name}",
                                        repo.trim_start_matches("oci://").trim_end_matches('/')
                                    )
                                } else {
                                    format!("{repo}|{name}")
                                },
                                if repo.starts_with("oci://") {
                                    Source::Oci
                                } else {
                                    Source::Helm
                                },
                                version,
                                &format!("{pointer}/helmCharts/{i}/version"),
                            )?;
                        }
                    }
                }
            }
            Manager::Flux => {
                if kind == "OCIRepository" {
                    if let Some(url) = doc.get("spec").and_then(|v| get(v, "url"))
                        && let Some(reference) = doc.get("spec").and_then(|v| v.get("ref"))
                        && let Some(tag) = reference.get("tag").or_else(|| reference.get("semver"))
                    {
                        let key = if reference.get("tag").is_some() {
                            "tag"
                        } else {
                            "semver"
                        };
                        let name = get(&doc["metadata"], "name").unwrap_or_else(|| url.clone());
                        scan.chart(
                            &name,
                            url.trim_start_matches("oci://").into(),
                            Source::Oci,
                            tag,
                            &format!("{pointer}/spec/ref/{key}"),
                        )?;
                    }
                } else if kind == "HelmRelease"
                    && let Some(spec) = doc
                        .get("spec")
                        .and_then(|v| v.get("chart"))
                        .and_then(|v| v.get("spec"))
                    && let (Some(name), Some(version), Some(source)) = (
                        get(spec, "chart"),
                        spec.get("version"),
                        spec.get("sourceRef"),
                    )
                {
                    let namespace = get(source, "namespace")
                        .or_else(|| get(&doc["metadata"], "namespace"))
                        .unwrap_or_default();
                    let repository = get(source, "name").and_then(|n| repos.get(&(namespace, n)));
                    if let Some((url, oci)) = repository {
                        let package = if *oci {
                            format!(
                                "{}/{}",
                                url.trim_start_matches("oci://").trim_end_matches('/'),
                                name
                            )
                        } else {
                            format!("{url}|{name}")
                        };
                        scan.chart(
                            &name,
                            package,
                            if *oci { Source::Oci } else { Source::Helm },
                            version,
                            &format!("{pointer}/spec/chart/spec/version"),
                        )?;
                    }
                }
                // chartRef releases keep the chart version in OCIRepository,
                // but still own image values here.
                if kind == "HelmRelease"
                    && let Some(values) = doc.get("spec").and_then(|v| v.get("values"))
                {
                    scan.images(values, &format!("{pointer}/spec/values"), true)?;
                }
            }
            _ => {}
        }
    }
    Ok(scan.out)
}
