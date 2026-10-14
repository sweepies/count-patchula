use crate::*;
use regex::Regex;
use std::str::FromStr;

pub(crate) struct Issue {
    pub field: String,
    pub message: String,
}

type Result<T = ()> = std::result::Result<T, Issue>;

fn fail<T>(field: &str, message: impl std::fmt::Display) -> Result<T> {
    Err(Issue {
        field: field.into(),
        message: format!("{field}: {message}"),
    })
}

fn regex(field: &str, pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|error| Issue {
        field: field.into(),
        message: format!("{field}: invalid regex: {error}"),
    })
}

fn pattern(field: &str, value: &str) -> Result {
    let value = value.strip_prefix('!').unwrap_or(value);
    if value.is_empty() {
        return fail(field, "pattern must not be empty");
    }
    if let Some(value) = value.strip_prefix("regex:") {
        regex(field, value)?;
    } else {
        let value = value.strip_prefix("glob:").unwrap_or(value);
        if value.is_empty() {
            return fail(field, "glob must not be empty");
        }
        globset::GlobBuilder::new(value)
            .literal_separator(true)
            .build()
            .map_err(|error| Issue {
                field: field.into(),
                message: format!("{field}: invalid glob: {error}"),
            })?;
    }
    Ok(())
}

fn patterns(field: &str, values: &[String]) -> Result {
    for (i, value) in values.iter().enumerate() {
        pattern(&format!("{field}[{i}]"), value)?;
    }
    Ok(())
}

fn filter(field: &str, value: &Option<String>) -> Result {
    let Some(value) = value else {
        return Ok(());
    };
    let value = value.strip_prefix('!').unwrap_or(value);
    if let Some(value) = value.strip_prefix("regex:") {
        regex(field, value)?;
    } else if let Some(value) = value.strip_prefix("semver:") {
        semver::VersionReq::parse(value).map_err(|error| Issue {
            field: field.into(),
            message: format!("{field}: invalid semver constraint: {error}"),
        })?;
    } else {
        return fail(field, "expected regex:... or semver:... version filter");
    }
    Ok(())
}

fn windows(field: &str, values: &[Window]) -> Result {
    for (i, window) in values.iter().enumerate() {
        let field = format!("{field}[{i}]");
        if window.cron.is_empty() {
            return fail(&field, "window must contain at least one cron expression");
        }
        if window.zone.parse::<chrono_tz::Tz>().is_err() {
            return fail(&field, format!("unknown IANA timezone {:?}", window.zone));
        }
        for cron in &window.cron {
            if cron.split_whitespace().count() != 5 {
                return fail(&field, "expected a five-field cron expression");
            }
            // cron's parser includes seconds; our public syntax fixes them to zero.
            cron::Schedule::from_str(&format!("0 {cron}")).map_err(|error| Issue {
                field: field.clone(),
                message: format!("{field}: invalid cron expression: {error}"),
            })?;
        }
    }
    Ok(())
}

fn required(field: &str, values: &[String]) -> Result {
    if values.is_empty() {
        return fail(field, "must contain at least one entry");
    }
    patterns(field, values)
}

fn template(field: &str, value: &str, captures: &Regex) -> Result {
    if value.is_empty() {
        return fail(field, "template must not be empty");
    }
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        if rest[..start].contains('}') {
            return fail(field, "unmatched } in template");
        }
        let Some(end) = rest[start + 1..].find('}') else {
            return fail(field, "unclosed {capture} in template");
        };
        let name = &rest[start + 1..start + 1 + end];
        if !captures
            .capture_names()
            .flatten()
            .any(|capture| capture == name)
        {
            return fail(
                field,
                format!("template references missing capture {{{name}}}"),
            );
        }
        rest = &rest[start + end + 2..];
    }
    if rest.contains('}') {
        return fail(field, "unmatched } in template");
    }
    Ok(())
}

impl Config {
    pub(crate) fn validate(&self) -> Result {
        patterns("ignore", &self.ignore)?;
        patterns("exclude", &self.exclude)?;
        filter("defaults.permitted", &self.defaults.permitted)?;
        windows("defaults.windows", &self.defaults.windows)?;
        for (i, rule) in self.rules.iter().enumerate() {
            let field = format!("rules[{i}]");
            for (name, values) in [
                ("packages", &rule.select.packages),
                ("dependencies", &rule.select.dependencies),
                ("files", &rule.select.files),
            ] {
                patterns(&format!("{field}.select.{name}"), values)?;
            }
            filter(&format!("{field}.select.current"), &rule.select.current)?;
            filter(&format!("{field}.apply.permitted"), &rule.apply.permitted)?;
            if let Some(value) = &rule.apply.windows {
                windows(&format!("{field}.apply.windows"), value)?;
            }
        }
        for (i, files) in self.files.iter().enumerate() {
            required(&format!("files[{i}].paths"), &files.paths)?;
        }
        for (i, extractor) in self.extractors.iter().enumerate() {
            let field = format!("extractors[{i}]");
            required(&format!("{field}.paths"), &extractor.paths)?;
            if extractor.patterns.is_empty() {
                return fail(&field, "extractor needs at least one regex pattern");
            }
            for (j, value) in extractor.patterns.iter().enumerate() {
                let capture = regex(&format!("{field}.patterns[{j}]"), value)?;
                if !capture
                    .capture_names()
                    .flatten()
                    .any(|name| name == "current")
                {
                    return fail(&field, "extractor regex needs a named `current` capture");
                }
                for (name, value) in [
                    ("source", &extractor.source),
                    ("dependency", &extractor.dependency),
                    ("package", &extractor.package),
                    ("scheme", &extractor.scheme),
                ] {
                    template(&format!("{field}.{name}"), value, &capture)?;
                }
                if !extractor.source.contains('{')
                    && serde_json::from_value::<Source>(serde_json::Value::String(
                        extractor.source.clone(),
                    ))
                    .is_err()
                {
                    return fail(&field, format!("unknown datasource {:?}", extractor.source));
                }
                if !extractor.scheme.contains('{')
                    && serde_json::from_value::<Scheme>(serde_json::Value::String(
                        extractor.scheme.clone(),
                    ))
                    .is_err()
                {
                    return fail(
                        &field,
                        format!("unknown version scheme {:?}", extractor.scheme),
                    );
                }
            }
            if let Some(value) = &extractor.extract {
                let capture = regex(&format!("{field}.extract"), value)?;
                if !capture
                    .capture_names()
                    .flatten()
                    .any(|name| name == "version")
                {
                    return fail(&field, "extract regex needs a named `version` capture");
                }
            }
        }
        if self.prs.concurrent == Some(0) || self.prs.per_hour == Some(0) {
            return fail(
                "prs",
                "PR limits must be positive integers or null (unlimited)",
            );
        }
        let branch = &self.prs.branch;
        if branch.is_empty()
            || branch.starts_with(['/', '.'])
            || branch.ends_with('.')
            || branch.contains(['~', '^', ':', '?', '*', '[', '\\'])
            || branch.chars().any(|c| c.is_whitespace() || c.is_control())
            || ["..", "@{", "//"].iter().any(|part| branch.contains(part))
            || format!("{branch}dependency")
                .split('/')
                .any(|part| part.starts_with('.') || part.ends_with(".lock"))
        {
            return fail("prs.branch", "expected a nonempty, valid Git branch prefix");
        }
        Ok(())
    }
}
