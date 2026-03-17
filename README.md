# Blue

A TOML-based declarative infrastructure deployment tool for [UpCloud](https://upcloud.com), written in Rust.

Define your cloud infrastructure in TOML, and Blue will plan, deploy, update, and destroy resources while tracking state and resolving dependencies automatically.

## Quick start

```sh
# Build
cargo build --release

# Validate your configuration
blue validate --file server.toml

# Preview what will change
blue plan --file server.toml

# Deploy
blue deploy --file server.toml

# Tear everything down
blue destroy --file server.toml
```

Blue requires the `UPCLOUD_TOKEN` environment variable to be set for any operation that contacts the UpCloud API (everything except `validate`).

## Configuration

Infrastructure is defined in TOML files. A configuration file has three sections: **parameters**, **data sources**, and **resources**.

### Parameters

Reusable variables with optional defaults, overridable from the CLI.

```toml
[parameters.disk_size]
description = "OS disk size in GB"
default = 10
```

Override at runtime:

```sh
blue plan --file server.toml --var disk_size=25
# or from a file
blue plan --file server.toml --var-file vars.toml
```

### Data sources

Query existing cloud objects to use their attributes in resource definitions.

```toml
[data.ubuntu]
type = "upcloud.storage"

[data.ubuntu.filters]
type = "template"
title = "Ubuntu Server 24.04 LTS (Noble Numbat)"
```

Results are available as `{{ data.<name>.<field> }}` in resource properties.

### Resources

The infrastructure to deploy.

```toml
[resources.web-01]
type = "upcloud.server"

[resources.web-01.properties]
title = "Web Server"
hostname = "web-01"
zone = "uk-lon1"
plan = "DEV-1xCPU-1GB"
metadata = true

[[resources.web-01.properties.storage_devices]]
action = "clone"
storage = "{{ data.ubuntu.uuid }}"
size = "{{ disk_size }}"
title = "web-01-os"
tier = "standard"

[[resources.web-01.properties.interfaces]]
type = "public"
ip_family = "IPv4"
```

### Variable interpolation

Use `{{ name }}` to reference parameters, data source outputs, or other resource outputs:

| Syntax | Resolves to |
|--------|-------------|
| `{{ disk_size }}` | Parameter value |
| `{{ data.ubuntu.uuid }}` | Data source output |
| `{{ resources.web-01.uuid }}` | Output from another resource |

Resource references create implicit dependencies — Blue automatically determines the correct creation order.

## Commands

### `validate`

Checks configuration syntax and validates resource properties against provider schemas. Does not contact the cloud API.

```sh
blue validate --file server.toml
```

### `plan`

Compares the desired configuration against the current state and shows what would change.

```sh
blue plan --file server.toml
blue plan --file server.toml --out changeset.json   # save for later deploy
```

### `deploy`

Applies changes. Can work from a live configuration file or a previously saved changeset.

```sh
blue deploy --file server.toml           # plan + apply in one step
blue deploy --plan changeset.json        # apply a saved plan
```

When using `--plan`, Blue checks that the state hasn't changed since the plan was created (via serial number). If it has, you'll need to re-run `plan`.

### `destroy`

Deletes all resources tracked in the state file, in reverse dependency order.

```sh
blue destroy --file server.toml
```

### `refresh`

Re-queries data sources and reads the current state of deployed resources from the cloud, updating the local state file without making any changes.

```sh
blue refresh --file server.toml
```

## State

blue tracks deployed infrastructure in a JSON state file (`blue.state.json` by default). This file records:

- The serial number (incremented on each change, used for stale-plan detection)
- Data source snapshots (resolved values from queries)
- Resource snapshots (type, status, properties, and outputs)

Resource statuses: `Creating`, `Ready`, `Failed`, `Deleting`.

Use `--state <path>` on any command to specify an alternate state file.

## Change detection

When planning or deploying, Blue diffs the desired configuration against the current state at the property level. Each resource change is one of:

| Symbol | Meaning |
|--------|---------|
| `+` | Create — new resource |
| `-` | Delete — resource removed from config |
| `~` | Update — properties changed in-place |
| `-/+` | Replace — an immutable property changed, requiring delete + recreate |

Properties marked `force_new` in the provider schema (like `zone`) trigger a full replacement when modified.

## Supported providers

### UpCloud

**Resources:**

| Type | Description |
|------|-------------|
| `upcloud.server` | Cloud server instance |

Server properties: `hostname`, `zone`, `plan`, `title`, `metadata`, `storage_devices`, `interfaces`.

**Data sources:**

| Type | Description |
|------|-------------|
| `upcloud.storage` | Query block storage / templates |

Filter by any storage field (`type`, `title`, `zone`, etc.). Title uses substring matching; other fields use exact matching.

## Project structure

```
src/
  main.rs              CLI definition and command dispatch
  cmd/
    mod.rs             Shared helpers (config resolution, display formatting)
    validate.rs        validate command
    plan.rs            plan command
    deploy.rs          deploy command
    destroy.rs         destroy command
    refresh.rs         refresh command
  config.rs            TOML loading, variable interpolation, parameter handling
  state.rs             State persistence, snapshots, and diffing
  deploy.rs            Deployment execution (create/update/delete orchestration)
  graph.rs             Dependency graph and topological sorting
  provider.rs          Provider trait and registry
  schema.rs            Schema definitions and validation
  providers/
    upcloud/
      mod.rs           UpCloud provider implementation
      server.rs        Server resource operations
      storage.rs       Storage data source queries
      schemas/
        server.toml    Server resource schema
        storage_data.toml  Storage data source schema
```
