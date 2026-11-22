//! PyPI's part in deciding where a Python package comes from: its public
//! hosts and name normalisation. The rule itself is `crate::origins`;
//! `settings` reads what a project configures and records.
use crate::origins::{Named, Public};
use crate::*;

pub(crate) mod settings;

/// PyPI, for `crate::origins`.
pub(crate) const PYPI: Public = Public {
    source: Source::Pypi,
    label: "https://pypi.org/simple",
    is_public: public,
};

/// What a Python index setting or lockfile entry names.
pub(crate) fn named(value: &str) -> Named {
    Named::parse(value, &PYPI)
}

/// Whether `url` is PyPI's own index, over https or plain http.
fn public(url: &str) -> bool {
    crate::web_host(url).is_some_and(|host| {
        matches!(
            host.as_str(),
            "pypi.org" | "pypi.python.org" | "files.pythonhosted.org"
        )
    })
}
