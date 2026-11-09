use crate::*;
use hcl_edit::{
    Span,
    expr::{Expression, Object},
    structure::Body,
};

pub(super) fn documents(files: &Files) -> Result<BTreeMap<&str, Body>, String> {
    files
        .iter()
        .filter(|(p, _)| p.ends_with(".tf") || p.ends_with(".terraform.lock.hcl"))
        .map(|(p, t)| {
            Ok((
                p.as_str(),
                t.read()
                    .map_err(|_| format!("{p}: the Count could not read it"))?
                    .parse()
                    .map_err(|e: hcl_edit::parser::Error| format!("{p}: {e}"))?,
            ))
        })
        .collect()
}
pub(super) fn extract(
    path: &str,
    text: &str,
    files: &BTreeMap<&str, Body>,
) -> Result<Vec<Dependency>, String> {
    let body = files.get(path).expect("selected HCL parsed once");
    let mut out = Vec::new();
    if path.ends_with(".terraform.lock.hcl") {
        return Ok(out);
    }
    for terraform in body.get_blocks("terraform") {
        if let Some(version) = terraform.body.get_attribute("required_version")
            && let Some(dep) = add(
                path,
                text,
                "hashicorp/terraform",
                "hashicorp/terraform",
                &version.value,
                DependencyKind::Toolchain,
                &mut out,
            )
        {
            dep.source = Source::GithubReleases;
            dep.extract = Some("^v?(?<version>.*)$".into());
        }
        for providers in terraform.body.get_blocks("required_providers") {
            for attribute in providers.body.attributes() {
                let name = attribute.key.as_str();
                if let Some(object) = attribute.value.as_object() {
                    let package = field(object, "source")
                        .and_then(Expression::as_str)
                        .map_or_else(|| format!("hashicorp/{name}"), str::to_owned);
                    if let Some(version) = field(object, "version") {
                        add(
                            path,
                            text,
                            name,
                            &package,
                            version,
                            DependencyKind::Provider,
                            &mut out,
                        );
                    }
                } else if attribute.value.as_str().is_some() {
                    add(
                        path,
                        text,
                        name,
                        &format!("hashicorp/{name}"),
                        &attribute.value,
                        DependencyKind::Provider,
                        &mut out,
                    );
                }
            }
        }
    }
    for module in body.get_blocks("module") {
        let source = module
            .body
            .get_attribute("source")
            .and_then(|a| a.value.as_str());
        let version = module.body.get_attribute("version");
        if let (Some(source), Some(version)) = (source, version)
            && !source.starts_with(['.', '/'])
            && !source.contains("::")
        {
            let name = module.labels.first().map(|v| v.as_str()).unwrap_or(source);
            add(
                path,
                text,
                name,
                source,
                &version.value,
                DependencyKind::Module,
                &mut out,
            );
        }
    }
    let base = path.rsplit_once('/').map_or("", |(p, _)| p);
    let lock_path = if base.is_empty() {
        ".terraform.lock.hcl".into()
    } else {
        format!("{base}/.terraform.lock.hcl")
    };
    if let Some(lock) = files.get(lock_path.as_str()) {
        for provider in lock.get_blocks("provider") {
            let Some(package) = provider
                .labels
                .first()
                .map(|v| provider_package(v.as_str()))
            else {
                continue;
            };
            let Some(version) = provider
                .body
                .get_attribute("version")
                .and_then(|a| a.value.as_str())
            else {
                continue;
            };
            for dep in &mut out {
                if dep.kind == DependencyKind::Provider && provider_package(&dep.package) == package
                {
                    dep.resolved = Some(version.into());
                }
            }
        }
    }
    Ok(out)
}
fn field<'a>(object: &'a Object, name: &str) -> Option<&'a Expression> {
    object.iter().find_map(|(key, value)| {
        let key = key
            .as_ident()
            .map(|v| v.as_str())
            .or_else(|| key.as_expr().and_then(Expression::as_str));
        (key == Some(name)).then_some(value.expr())
    })
}
fn add<'a>(
    path: &str,
    text: &str,
    name: &str,
    package: &str,
    version: &Expression,
    kind: DependencyKind,
    out: &'a mut Vec<Dependency>,
) -> Option<&'a mut Dependency> {
    let (Some(current), Some(span)) = (version.as_str(), version.span()) else {
        return None;
    };
    out.push(Dependency {
        manager: Manager::Terraform,
        file: path.into(),
        location: Location::new(
            text,
            span.start,
            span.end,
            format!("/{}/{name}/version", kind.as_str()),
        ),
        form: EditForm::Scalar(Syntax::Json),
        name: name.into(),
        package: provider_package(package).into(),
        kind,
        current: current.into(),
        resolved: None,
        digest: None,
        source: Source::Terraform,
        scheme: Scheme::Hashicorp,
        extract: None,
        included_by: None,
        registry: None,
    });
    out.last_mut()
}

fn provider_package(package: &str) -> &str {
    package
        .strip_prefix("registry.terraform.io/")
        .or_else(|| package.strip_prefix("registry.opentofu.org/"))
        .unwrap_or(package)
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn selected(&self, _config: &Config, path: &str) -> bool {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        leaf.ends_with(".tf") || matches!(leaf, ".terraform.lock.hcl" | ".terraform-version")
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        let documents = documents(files)?;
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                let result = if path.ends_with(".terraform-version") {
                    Ok(toolchains::terraform(path, text))
                } else {
                    extract(path, text, &documents)
                };
                file_result(out, path, result);
                Ok(())
            })),
            finish: None,
        })
    }
}
