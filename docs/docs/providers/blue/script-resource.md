# script (resource)

Runs a user-defined script as its create operation. State-tracked with standard diffing and cascade logic.

## Usage

::: v-pre
```toml
[resources.random_id]
type = "blue.script"
script = "scripts/generate_id.js"
triggers_replace = { server_name = "{{ resources.web-01.hostname }}" }
inputs = { region = "{{ parameters.region }}" }
```
:::

## Inputs

| Field | Type | Required | force_new | Description |
|---|---|---|---|---|
| `script` | string | yes | yes | Path to the script file, relative to the config file |
| `triggers_replace` | object | no | yes | Key-value pairs that trigger replacement when changed. **Not** passed to the script body. |
| `inputs` | object | no | yes | Key-value pairs passed to the script body as the `inputs` variable. Changes trigger replacement (script re-runs). |

The script file hash is also tracked — modifying the script file content triggers replacement even if the path stays the same.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | Runs the script, returns outputs |
| **read** | Returns stored outputs (no external state) |
| **update** | No-op (inputs updated in state, script not re-run) |
| **delete** | No-op |

## When does the script re-run?

The script only runs on **create** (including replacement). Changing inputs that are not `force_new` updates state but does not re-run the script. To force a re-run, change a `triggers_replace` value or modify the script file.

## Script format

Same as [script data sources](./script-data-source.md#script-format) — Deno runtime, `inputs` variable, `return` for output.
