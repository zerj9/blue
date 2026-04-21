# Blue

Infrastructure as Code in TOML. Define resources declaratively, and Blue handles dependency resolution, diffing, and deployment.

### Define resources

```toml
# infra.toml
[parameters.name]
default = "my-server"

[resources.example]
type = "blue.script"

[resources.example.inputs]
script = "setup.js"
triggers_replace = { name = "{{ parameters.name }}" }
```

### Plan and deploy

```bash
# Preview changes
blue plan -f infra.toml

# Apply changes
blue deploy -f infra.toml

# Override parameters
blue deploy -f infra.toml --var name=production

# Load parameters from a file
blue deploy -f infra.toml --var-file vars.toml
```

### Manage state

```bash
# Sync state with live provider values
blue refresh

# Tear down all managed resources
blue destroy
```

## Commands

| Command   | Description |
|-----------|-------------|
| `plan`    | Build dependency graph, diff config against state, produce a plan |
| `deploy`  | Execute a plan to create, update, or delete resources |
| `refresh` | Update state with live values from providers |
| `destroy` | Delete all managed resources |

## Configuration

Resources are defined in TOML files. Each resource has a `type` and `inputs`:

```toml
[resources.web]
type = "blue.script"
depends_on = ["resources.db"]

[resources.web.inputs]
script = "create_server.js"
triggers_replace = { version = "2" }
```

### Template references

Reference outputs from other resources using `{{ }}` syntax:

```toml
[resources.app.inputs]
server_id = "{{ resources.web.uuid }}"
```

Blue resolves dependencies automatically from template references and `depends_on` declarations, then executes operations in topological order.

### Parameters

Define parameters with optional defaults:

```toml
[parameters.region]
default = "eu-west-1"

[parameters.env]
# no default — must be provided via --var or --var-file
```

### Data sources

Data sources fetch external data without managing lifecycle:

```toml
[data_sources.config]
type = "blue.script"

[data_sources.config.inputs]
script = "fetch_config.js"
```

Data source outputs can be referenced in resource inputs:

```toml
[resources.app.inputs]
api_key = "{{ data_sources.config.key }}"
```

## State

State is stored in `blue.state.json` (configurable via `--state`). It tracks:

- Resource outputs and inputs
- Dependency relationships
- Serial number and lineage for staleness detection

## Development

```bash
cargo build
cargo test
```
