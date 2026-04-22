# Configuration

Blue uses two TOML configuration files:

- **Resource config** — defines your infrastructure: parameters, data sources, resources, and encryption settings. This is the main file you work with.
- **Provider config** (`blue.providers.toml`) — defines how Blue connects to cloud providers: credentials, account aliases, and authentication. Kept separate so it can be committed safely (no secrets in the file).

## Resource config

The resource config file is passed to Blue via the `-f` flag:

```sh
blue plan -f my-infra.toml
```

It contains four top-level sections:

| Section | Purpose |
|---|---|
| `[parameters.*]` | Input values — CLI overrides, env vars, defaults |
| `[data.*]` | Read-only lookups — provider queries and scripts |
| `[resources.*]` | Managed infrastructure — provider resources and scripts |
| `[encryption]` | Secret encryption settings |

### Example

::: v-pre
```toml
[encryption]
recipients = ["age1abc..."]

[parameters.github_token]
description = "GitHub PAT for server setup"
secret = true
env = "GITHUB_TOKEN"

[data.ubuntu]
type = "upcloud.storage"
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }

[resources.web-01]
type = "upcloud.server"

[resources.web-01.inputs]
hostname = "web-01"
zone = "uk-lon1"
plan = "DEV-1xCPU-1GB"
storage = "{{ data.ubuntu.uuid }}"
user_data = "#!/bin/bash\necho '{{ parameters.github_token }}'"
```
:::

## Provider config

See [Providers](./providers.md) for the full reference.

## Sections

- [Parameters](./parameters.md)
- [Data Sources](./data-sources.md)
- [Resources](./resources.md)
- [Templates](./templates.md)
- [Encryption](./encryption.md)
- [Providers](./providers.md)
