# Providers

The provider config file defines how Blue connects to cloud providers. It is separate from the resource config — kept in its own file so it can be committed safely without exposing secrets.

## File location

Default: `providers.toml` in the working directory. Override with the `--providers` flag:

```sh
blue plan -f config.toml --providers path/to/providers.toml
```

## Format

Each top-level table (except `[data.*]`) defines a provider instance:

```toml
[upcloud]
type = "upcloud"
username_env = "UPCLOUD_USERNAME"
password_env = "UPCLOUD_PASSWORD"
```

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | The provider type (e.g. `"upcloud"`) |
| Other fields | | | Provider-specific — see the provider documentation |

Credentials can be supplied via environment variable references (`*_env` fields) or template references to script data sources.

## Multiple instances

You can configure multiple instances of the same provider with different credentials:

```toml
[upcloud]
type = "upcloud"
username_env = "UPCLOUD_EU_USERNAME"
password_env = "UPCLOUD_EU_PASSWORD"

[upcloud-us]
type = "upcloud"
username_env = "UPCLOUD_US_USERNAME"
password_env = "UPCLOUD_US_PASSWORD"
```

Reference a specific instance with the `provider` field on resources and data sources:

```toml
[resources.web-eu]
type = "upcloud.server"
# uses "upcloud" instance (default — matches type prefix)

[resources.web-us]
type = "upcloud.server"
provider = "upcloud-us"
# uses "upcloud-us" instance
```

If no `provider` field is specified, Blue uses the instance whose name matches the prefix of the `type` field.

## Script data sources in provider config

Provider config can include script data sources for fetching credentials from external systems like secret managers:

::: v-pre
```toml
[data.vault_creds]
type = "blue.script"
script = "scripts/fetch_vault_creds.js"
inputs = { path = "secret/upcloud" }

[upcloud]
type = "upcloud"
username = "{{ data.vault_creds.username }}"
password = "{{ data.vault_creds.password }}"
```
:::

These script data sources are resolved at startup before provider instances are built. They can only reference other script data sources and environment variables — not provider data sources (you can't use a provider to fetch its own credentials).

## Validation

- Every `provider` field on a resource or data source must reference a provider instance defined in the provider config.
- The referenced instance's `type` must match the provider prefix of the resource's `type`. For example, a resource with `type = "upcloud.server"` and `provider = "my-instance"` requires `my-instance` to have `type = "upcloud"`.
