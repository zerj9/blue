# Parameters

Parameters are input values for your configuration. They can come from CLI flags, environment variables, default values, or interactive prompts.

## Definition

```toml
[parameters.name]
description = "Human-readable description"
default = "value"
secret = false
env = "ENV_VAR_NAME"
```

## Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `description` | string | no | Human-readable description, shown when prompting |
| `default` | any | no | Default value if no other source provides one |
| `secret` | bool | no | If `true`, value is masked when prompting and encrypted in state/plan files. Defaults to `false` |
| `env` | string | no | Environment variable name to read the value from |

## Resolution order

When resolving a parameter's value, Blue checks sources in this order (first match wins):

1. **CLI `--var` flag** — `blue plan -f config.toml --var name=value`
2. **Variable file** — `blue plan -f config.toml --var-file vars.toml`
3. **Environment variable** — if the `env` field is set
4. **Default value** — if the `default` field is set
5. **Interactive prompt** — if none of the above, Blue prompts the user (masked input if `secret = true`)

If no value can be resolved and the parameter has no default, Blue prompts the user interactively. If the parameter has `secret = true`, input is masked.

### Variable file format

The `--var-file` flag accepts a TOML file with key-value pairs:

```toml
# vars.toml
region = "uk-lon1"
disk_size = 20
```

```sh
blue plan -f config.toml --var-file vars.toml
```

## Types

The parameter type is inferred from the `default` value:

| Default value | Inferred type |
|---|---|
| `"hello"` | string |
| `42` | integer |
| `3.14` | float |
| `true` / `false` | boolean |

Values from CLI flags and environment variables are strings. Blue coerces them to the expected type when possible.

## Referencing parameters

Use <code v-pre>{{ parameters.name }}</code> in resource inputs and data source inputs:

::: v-pre
```toml
[parameters.disk_size]
description = "OS disk size in GB"
default = 10

[resources.web-01.inputs]
size = "{{ parameters.disk_size }}"
```
:::

When the reference is the entire value, the original type is preserved (integer stays integer). When embedded in a larger string, it's stringified. See [Templates](./templates.md) for details.

## Examples

### Secret parameter from environment

```toml
[parameters.api_key]
description = "API key for external service"
secret = true
env = "API_KEY"
```

### Parameter with default

```toml
[parameters.region]
description = "Deployment region"
default = "uk-lon1"
```
