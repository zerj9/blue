# Resources

Resources are managed infrastructure. Blue tracks their state and handles their full lifecycle: create, read, update, and delete.

## Provider resource

A provider resource manages infrastructure through a cloud API:

::: v-pre
```toml
[resources.web-01]
type = "upcloud.server"
provider = "upcloud-us"          # optional, defaults to provider from type
hostname = "web-01"
zone = "uk-lon1"
plan = "DEV-1xCPU-1GB"
storage = "{{ data.ubuntu.uuid }}"
```
:::

### Fields

`type` and `provider` are reserved keys read by Blue. Every other key is a resource input — its valid set, types, and required-ness depend on the resource type. See the provider's documentation for each resource type's input list.

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Provider and resource type in `provider.type` format |
| `provider` | string | no | Provider instance name. Defaults to the prefix of `type` |
| _other keys_ | varies | varies | Resource inputs — see the provider documentation |

## Script resource

A script resource runs a user-defined script as its create operation. It behaves like a regular resource — state-tracked, with standard diffing and cascade logic:

::: v-pre
```toml
[resources.random_id]
type = "blue.script"
script = "scripts/generate_id.js"
triggers_replace = { server_name = "{{ resources.web-01.hostname }}" }
```
:::

### Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"blue.script"` |
| `script` | string | yes | Path to the script file, relative to the config file |
| `triggers_replace` | table | no | Key-value pairs that trigger replacement when changed |

### Lifecycle

- **create** — runs the script and returns outputs
- **read** — returns stored outputs (no external state)
- **update** — no-op (inputs updated in state, script not re-run)
- **delete** — no-op

### Replacement

`triggers_replace` values and the script file hash are `force_new` fields. If either changes, the script resource is replaced (deleted and recreated), which re-runs the script. Standard cascade logic applies — downstream resources that reference this resource's outputs in `force_new` fields are also replaced.

### When does the script re-run?

The script only runs on **create** (including replacement). Changing inputs that are not in `triggers_replace` updates state but does not re-run the script. To force a re-run, change a `triggers_replace` value or modify the script file.

### Script format

Same as [script data sources](./data-sources.md#script-format) — Deno runtime, `inputs` variable, `return` for output.

## Referencing resource outputs

Use <code v-pre>{{ resources.name.path }}</code> to reference a resource's outputs. Refs access **outputs only** — the values returned by the provider after create/read/update — not inputs.

::: v-pre
```toml
[resources.web-01-firewall]
type = "upcloud.firewall"
server_uuid = "{{ resources.web-01.uuid }}"
```
:::

See [Templates](./templates.md) for the full reference syntax including array indexing and filters.

## Dependencies

Dependencies are derived automatically from <code v-pre>{{ }}</code> references. If resource B references resource A's output, Blue ensures A is created before B and updated/replaced when A changes.

You don't need to declare dependencies explicitly — they follow from your template references.

::: warning
Removing a resource from config while another resource still references its outputs is a validation error. Remove or update the references first.
:::

## Change behavior

When you modify a resource's inputs and run `plan`, Blue diffs the resolved inputs against the values stored in state. The action depends on which inputs changed and their schema metadata.

### Update

If a changed input has no special metadata, the resource is updated in place. The provider's `update` method is called with the new values.

### Replacement (force_new)

Some inputs can't be changed in place — the resource must be deleted and recreated. These inputs are marked `force_new` in the provider schema. If a `force_new` input changes, the plan shows a **replace** action.

Check the provider documentation for each resource type to see which inputs are `force_new`.

### Restart (requires_stop)

Some inputs require the resource to be stopped before updating. These are marked `requires_stop` in the provider schema. The provider handles the stop, update, and restart cycle automatically — this is transparent to the user. The plan shows this as a regular **update** with a note that a restart is required.

### Array ordering

Array inputs in the schema declare whether order matters:

- **Ordered** (default) — positional comparison. Reordering elements causes a diff. Used when order matters (e.g. firewall rules).
- **Unordered** — set comparison. Order is ignored. Used when order doesn't matter (e.g. tags).

Check the provider documentation for each resource type to see which array inputs are ordered.

### Cascade replacements

When a resource is replaced, Blue checks its dependents. If a dependent resource references the replaced resource's outputs in a `force_new` input, the dependent is also replaced — the upstream outputs will change, and the input requires a new resource.

Dependents that only reference the replaced resource in non-`force_new` inputs are updated instead — they receive the new values in place.

**Example:** Resource A (server) is replaced due to a `force_new` change. Resource B (firewall rules) references A's UUID in a `force_new` field (`server_uuid`) — B is also replaced. Resource C (DNS record) references A's IP in a non-`force_new` field — C is updated in place with the new IP.

This cascade is determined entirely at plan time. Deploy receives the full list of operations with cascades already resolved.

### Retry

Some resource types support automatic retries on failure. This is configured in the provider schema — not in your config. When a resource operation fails and the schema has retry config, deploy retries automatically before giving up.

Check the provider documentation to see which resource types support retries.
