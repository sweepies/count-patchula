"""The Count's Poetry launcher: Poetry without any way to build a package.

Poetry reads a package's metadata by building it (a PEP 517 metadata hook
in an isolated environment) whenever the metadata is not static, and no
setting turns that off for `poetry lock`. This launcher replaces every
entry point that leads to a build, or to loading a plugin, with one that
stops the process before anything runs, then starts Poetry's own `main`.

    python -I poetry_guard.py <poetry version> <poetry arguments...>

It first finds the interpreter Poetry is installed under from the `poetry`
script on PATH and restarts itself there, isolated (`-I`: no current
directory, script directory or PYTHON* variables on the path).

Exit status 86: Poetry would have built a package or loaded a plugin.
Exit status 87: this is not a Poetry the guard covers (another version, or
an entry point it replaces is missing), so it does not run at all.
"""

import importlib
import importlib.metadata
import os
import shutil
import sys

REFUSED = 86
UNGUARDED = 87
STAGE = "--patchula-guarded"

# (module, class or None, attribute): every way the Poetry this guard
# covers can start a build or load code that is not Poetry's own.
GUARDED = [
    # The PEP 517 hook call itself, through which every build backend runs.
    ("pyproject_hooks._impl", "BuildBackendHookCaller", "_call_hook"),
    # pypa/build's builder, which Poetry's isolated builds construct.
    ("build._builder", "ProjectBuilder", "__init__"),
    # Poetry's isolated build environment, which also installs the build
    # requirements a backend names.
    ("poetry.utils.isolated_build", None, "isolated_builder"),
    # Metadata by building, when a package's own metadata is not static.
    ("poetry.inspection.info", None, "get_pep517_metadata"),
    # The installer's sdist builder.
    ("poetry.installation.chef", "Chef", "prepare"),
    # Plugins, installed or the project's own.
    ("poetry.plugins.plugin_manager", "PluginManager", "load_plugins"),
    ("poetry.plugins.plugin_manager", "PluginManager", "add_project_plugin_path"),
    ("poetry.plugins.plugin_manager", "PluginManager", "ensure_project_plugins"),
]


def stop(status, message):
    sys.stderr.write(f"patchula: {message}\n")
    sys.stderr.flush()
    # Not an exception: nothing in Poetry can catch it and carry on.
    os._exit(status)


def restart(expected, args):
    """Start this file again under the interpreter Poetry is installed in."""
    script = shutil.which("poetry")
    if script is None:
        stop(UNGUARDED, "no poetry on PATH")
    with open(script, "rb") as handle:
        first = handle.readline().decode("utf-8", "replace").strip()
        second = handle.readline().decode("utf-8", "replace").strip()
    interpreter = first[2:].strip() if first.startswith("#!") else ""
    # The installer's trampoline for an interpreter path too long for a
    # shebang: `#!/bin/sh` then `'''exec' "<python>" "$0" "$@"`.
    prefix, suffix = "'''exec' \"", '" "$0" "$@"'
    if interpreter == "/bin/sh" and second.startswith(prefix) and second.endswith(suffix):
        interpreter = second[len(prefix) : -len(suffix)]
    if not os.path.isabs(interpreter) or not os.path.isfile(interpreter):
        stop(UNGUARDED, "the poetry script names no interpreter the guard can use")
    here = os.path.abspath(__file__)
    os.execv(interpreter, [interpreter, "-I", here, STAGE, expected, *args])


def refuser(name):
    def refuse(*args, **kwargs):
        stop(
            REFUSED,
            f"Poetry reached {name}, which builds a package or loads a plugin; refused",
        )

    return refuse


def guard():
    originals = []
    for module_name, owner_name, attribute in GUARDED:
        try:
            module = importlib.import_module(module_name)
        except Exception as error:
            stop(UNGUARDED, f"cannot import {module_name}: {error}")
        owner = module if owner_name is None else getattr(module, owner_name, None)
        if owner is None or not hasattr(owner, attribute):
            name = module_name if owner_name is None else f"{module_name}.{owner_name}"
            stop(UNGUARDED, f"{name} has no {attribute}")
        qualified = ".".join(filter(None, [module_name, owner_name, attribute]))
        original = getattr(owner, attribute)
        replacement = refuser(qualified)
        setattr(owner, attribute, replacement)
        originals.append((original, replacement))
    # A module imported along the way may hold its own reference to a
    # function from `from x import y`: replace every one of those too.
    for module in list(sys.modules.values()):
        namespace = getattr(module, "__dict__", None)
        if not isinstance(namespace, dict):
            continue
        for key, value in list(namespace.items()):
            for original, replacement in originals:
                if value is original:
                    namespace[key] = replacement


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == STAGE:
        expected, args = sys.argv[2], sys.argv[3:]
    else:
        if len(sys.argv) < 2:
            stop(UNGUARDED, "usage: poetry_guard.py <poetry version> <arguments...>")
        restart(sys.argv[1], sys.argv[2:])
        return
    try:
        installed = importlib.metadata.version("poetry")
    except importlib.metadata.PackageNotFoundError:
        stop(UNGUARDED, "Poetry is not installed under this interpreter")
    if installed != expected:
        stop(UNGUARDED, f"Poetry {installed} is not the {expected} the guard covers")
    guard()
    from poetry.console.application import main as poetry

    sys.argv = ["poetry", *args]
    sys.exit(poetry())


if __name__ == "__main__":
    main()
