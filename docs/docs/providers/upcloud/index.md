# UpCloud (Provider)

The `upcloud` provider talks to the [UpCloud](https://upcloud.com) API for managing cloud infrastructure: servers, storage, networking. Provider instances must be defined in the provider config file before resources or data sources can use them.

## Authentication

Two mutually exclusive auth modes are supported.

### API token (recommended)

UpCloud's API supports HTTP Bearer tokens. Generate one in the UpCloud control panel; see [UpCloud's API token docs](https://developers.upcloud.com/1.3/23-tokens/) for details.

```toml
[upcloud]
type = "upcloud"
token_env = "UPCLOUD_TOKEN"
```

Or supply the token literally (less common — keeps the secret on disk):

```toml
[upcloud]
type = "upcloud"
token = "ucat_..."
```

### Username and password

```toml
[upcloud]
type = "upcloud"
username_env = "UPCLOUD_USERNAME"
password_env = "UPCLOUD_PASSWORD"
```

Or directly:

```toml
[upcloud]
type = "upcloud"
username = "..."
password = "..."
```

## Configuration fields

| Field | Type | Description |
|---|---|---|
| `type` | string | Must be `"upcloud"` |
| `token` / `token_env` | string | API token, or the name of an environment variable to read it from |
| `username` / `username_env` | string | API username (must pair with `password`) |
| `password` / `password_env` | string | API password (must pair with `username`) |

Each credential field accepts either a literal value (`<field>`) or an environment-variable reference (`<field>_env`). Setting both forms of the same field is an error.

The two auth modes are mutually exclusive: configuring both `token` and `username`/`password` is an error, and configuring neither is an error.

## Multiple instances

Configure multiple UpCloud instances with different credentials to deploy across accounts or regions:

::: v-pre
```toml
[upcloud]
type = "upcloud"
token_env = "UPCLOUD_PROD_TOKEN"

[upcloud-staging]
type = "upcloud"
token_env = "UPCLOUD_STAGING_TOKEN"
```
:::

Reference a specific instance from a resource or data source via the `provider` field:

```toml
[resources.web-staging]
type = "upcloud.server"
provider = "upcloud-staging"
```

If `provider` is omitted, the instance whose name matches the type prefix is used — `type = "upcloud.server"` defaults to the `[upcloud]` instance.

See [Provider configuration](../../config/providers.md) for the cross-provider mechanics (the `provider` field, the relationship between `type` and instance name, and validation rules).
