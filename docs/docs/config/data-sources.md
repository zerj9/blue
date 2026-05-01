# Data Sources

Data sources are read-only lookups that run every plan. They fetch information from providers or execute scripts, and their outputs can be referenced by resources and other data sources.

Data sources have no state — they always run fresh. They may only depend on parameters and other data sources, not on resources.

## Provider data source

A data source queries the provider to look up existing information:

```toml
[data.ubuntu]
type = "upcloud.storage"
provider = "upcloud-us"          # optional, defaults to provider from type
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Provider and data source type in `provider.type` format |
| `provider` | string | no | Provider instance name. Defaults to the prefix of `type` |
| `filters` | table | no | Key-value pairs to filter results |

## Script data source

A script data source runs a user-defined script:

```toml
[data.vault_creds]
type = "blue.script"
script = "scripts/fetch_creds.js"
inputs = { path = "secret/upcloud" }
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"blue.script"` |
| `script` | string | yes | Path to the script file, relative to the config file |
| `inputs` | table | no | Key-value pairs passed to the script as the `inputs` variable |

### Script format

Scripts run in a shipped Deno runtime. The script body is wrapped in a function with `inputs` available as a parameter. Use `return` to produce output:

```js
// scripts/fetch_creds.js
const response = await fetch(`https://vault.example.com/v1/${inputs.path}`);
const data = await response.json();
return {
  username: data.username,
  password: data.password,
};
```

Early returns are supported. Stderr is used for logging.

## Referencing data source outputs

Use <code v-pre>{{ data.name.path }}</code> in resource inputs:

::: v-pre
```toml
[data.ubuntu]
type = "upcloud.storage"
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }

[resources.web-01]
type = "upcloud.server"
storage = "{{ data.ubuntu.uuid }}"
```
:::

See [Templates](./templates.md) for the full reference syntax.

## Caching

Data sources have no caching — they run fresh on every `plan`. If a data source calls an expensive external API, every plan incurs that cost.

## Dependency constraint

Data sources may only depend on parameters and other data sources — not on resources. This ensures all data source values are known at plan time. If a script needs a resource output as input, use a [script resource](./resources.md#script-resource) instead.
