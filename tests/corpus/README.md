# Dependency corpus

Real manifests from permissively licensed projects, copied at fixed commits.
They give the Count a large, varied dependency set to chew on in CI: about 2,800
dependencies across 23 managers.

- `mise run test` extracts the whole corpus offline and compares it with
  `tests/corpus-extraction.tsv`. A change to extraction shows up as a diff
  in that file. Regenerate it with `mise run corpus:snapshot`.
- On pull requests, CI also looks up every dependency against the live
  registries and plans every branch without writing (`mise run corpus:check`,
  which is `patchula run --dry-run` in `tests/corpus`). A branch that fails
  to plan fails the job; counts, skips and timings go in the job summary.

Each directory keeps its project's own relative paths. The `.github/workflows`
directory holds Home Assistant's workflows at the root, where the Actions
manager looks for them; `.forgejo/workflows` and `.gitea/workflows` hold
Forgejo and Gitea Actions workflows the same way. Woodpecker workflows sit at
the root for the same reason: Woodpecker's own `.woodpecker` directory, and
webtail's `.woodpecker.yml` in the older `pipeline:` shape. The files are
unmodified copies; their licences are those of the projects below.

| Source | Commit | Licence | Directory |
|---|---|---|---|
| [vercel/next.js](https://github.com/vercel/next.js) | `72895219d44d` | MIT | `javascript/next.js` |
| [astral-sh/uv](https://github.com/astral-sh/uv) | `dce159bf3c49` | Apache-2.0 | `rust/uv` |
| [prometheus/prometheus](https://github.com/prometheus/prometheus) | `30bcf0f03c33` | Apache-2.0 | `go/prometheus` |
| [pydantic/pydantic](https://github.com/pydantic/pydantic) | `e3b6f55a8c77` | MIT | `python/pydantic` |
| [home-assistant/core](https://github.com/home-assistant/core) | `a38890c0da2f` | Apache-2.0 | `python/home-assistant` |
| [apache/superset](https://github.com/apache/superset) | `abc1b50f65dc` | Apache-2.0 | `containers/superset` |
| [prometheus-community/helm-charts](https://github.com/prometheus-community/helm-charts) | `740ca6d94cf1` | Apache-2.0 | `kubernetes/kube-prometheus-stack` |
| [terraform-aws-modules/terraform-aws-eks](https://github.com/terraform-aws-modules/terraform-aws-eks) | `91b8a8d944e3` | Apache-2.0 | `terraform/eks` |
| [spring-projects/spring-petclinic](https://github.com/spring-projects/spring-petclinic) | `500158f73241` | Apache-2.0 | `jvm/spring-petclinic` |
| [apache/kafka](https://github.com/apache/kafka) | `9fbcd3f76cec` | Apache-2.0 | `jvm/kafka` |
| [dotnet/aspire](https://github.com/dotnet/aspire) | `f4ae2cc424d5` | MIT | `dotnet/aspire` |
| [rails/rails](https://github.com/rails/rails) | `f51657c4e4e5` | MIT | `ruby/rails` |
| [laravel/framework](https://github.com/laravel/framework) | `2184bbe43573` | MIT | `php/laravel` |
| [jdx/mise](https://github.com/jdx/mise) | `60c6a39f36b5` | MIT | `tools/mise` |
| [microsoft/vscode](https://github.com/microsoft/vscode) | `f82f4adff5e0` | MIT | `editors/vscode` |
| [home-assistant/core](https://github.com/home-assistant/core) | `a38890c0da2f` | Apache-2.0 | `.github/workflows` |
| [woodpecker-ci/woodpecker](https://github.com/woodpecker-ci/woodpecker) | `ae3b942fa228` | Apache-2.0 | `.woodpecker` (YAML files only) |
| [LeKovr/webtail](https://github.com/LeKovr/webtail) | `6ee527cd624a` | MIT | `.woodpecker.yml` |
| [actions/forgejo-release](https://code.forgejo.org/actions/forgejo-release) | `585a6614e80a` | MIT | `.forgejo/workflows` (`integration.yml`, `release.yml`, `tests.yml`) |
| [actions/setup-forgejo](https://code.forgejo.org/actions/setup-forgejo) | `3e9e3e86161a` | MIT | `.forgejo/workflows` (`cascade-end-to-end.yml`, `integration-*.yml`) |
| [gitea/tea](https://gitea.com/gitea/tea) | `2727270931ca` | MIT | `.gitea/workflows` |
| [pawamoy/duty](https://github.com/pawamoy/duty) | `f843de3442b9` | ISC | `python/duty` (`.copier-answers.yml` only) |
| [mkdocstrings/python](https://github.com/mkdocstrings/python) | `50d135aa5621` | ISC | `python/mkdocstrings-python` (`.copier-answers.yml` only) |
| [jupyterlab/jupyterlab-git](https://github.com/jupyterlab/jupyterlab-git) | `e77defa11293` | BSD-3-Clause | `javascript/jupyterlab-git` (`.copier-answers.yml` only) |
| [gitlab-org/gitaly](https://gitlab.com/gitlab-org/gitaly) | `42a4812bc672` | MIT | `ci/gitaly` |
| [gitlab-org/container-registry](https://gitlab.com/gitlab-org/container-registry) | `672bd7536c95` | Apache-2.0 | `ci/container-registry` |
| [gitlab-org/cli](https://gitlab.com/gitlab-org/cli) | `5419ed460090` | MIT | `ci/cli` |
