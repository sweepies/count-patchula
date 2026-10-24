# Dependency corpus

Real manifests from permissively licensed projects, copied at fixed commits.
They give the Count a large, varied dependency set to chew on in CI: about 2,600
dependencies across 19 managers.

- `mise run test` extracts the whole corpus offline and compares it with
  `tests/corpus-extraction.tsv`. A change to extraction shows up as a diff
  in that file. Regenerate it with `mise run corpus:snapshot`.
- On pull requests, CI also looks up every dependency against the live
  registries and plans every branch without writing (`mise run corpus:check`,
  which is `patchula run --dry-run` in `tests/corpus`). A branch that fails
  to plan fails the job; counts, skips and timings go in the job summary.

Each directory keeps its project's own relative paths. The `.github/workflows`
directory holds Home Assistant's workflows at the root, where the Actions
manager looks for them. The files are unmodified copies; their licences are
those of the projects below.

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
