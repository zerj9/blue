# CLI Reference

## plan

Build the dependency graph, diff config against state, and produce a plan.

```sh
blue plan -f config.toml
```

### Flags

| Flag | Description |
|---|---|
| `-f, --file` | Path to the resource config file (required) |
| `--providers` | Path to provider config file (default: `blue.providers.toml`) |
| `--state` | Path to state file (default: `blue.state.json`) |
| `--out` | Save the plan to a file for later deployment |
| `--var KEY=VALUE` | Override a parameter value (repeatable) |
| `--var-file FILE` | Load parameter overrides from a TOML file |

### What it does

1. **Refresh** — runs the same logic as the `refresh` command: calls `read` on all existing resources to get live state from providers
2. **Build graph** — combines config and state into a dependency graph
3. **Traverse** — resolves parameters, runs data sources, diffs resources against state
4. **Cascade** — propagates replacements to dependent resources
5. **Output** — displays the plan and optionally saves it to a file

### Output

The plan shows each resource and its action:

- **create** — resource is in config but not in state
- **update** — resource inputs changed
- **replace** — a `force_new` property changed (delete + create)
- **delete** — resource is in state but not in config
- **unchanged** — no changes detected

## deploy

Execute a plan to create, update, or delete resources.

```sh
blue deploy plan.json
```

Or run plan and deploy in one step:

```sh
blue deploy -f config.toml
```

### Flags

| Flag | Description |
|---|---|
| `-f, --file` | Path to the resource config file (runs plan first, then deploys) |
| `--providers` | Path to provider config file (default: `blue.providers.toml`) |
| `--state` | Path to state file (default: `blue.state.json`) |
| `--var KEY=VALUE` | Override a parameter value (repeatable, only with `-f`) |
| `--var-file FILE` | Load parameter overrides from a TOML file (only with `-f`) |

### Saved plans

When you save a plan with `--out`, some property values are stored as references to other resources in the plan rather than concrete values. These references are resolved during deploy as each operation completes and produces outputs. This means a saved plan is tied to the state it was created from — see staleness check below.

### Staleness check

Before executing, deploy verifies the state file hasn't changed since the plan was created (using lineage UUID and serial number). Any state change — including another deploy, refresh, or manual edit — invalidates the plan. Re-run `plan` to get a fresh plan.

### Error handling

Deploy executes operations in order. If any operation fails, it stops immediately — no skipping, no continuing independent branches.

Blue doesn't roll back. It saves whatever outputs were produced and stops. On the next `plan`, Blue calls `read` on all resources to reconcile state with reality.

**What to do when deploy fails:**

1. Check the error message
2. Fix the issue (provider config, permissions, etc.)
3. Run `plan` again — it will refresh state and show what's left to do
4. Run `deploy` with the new plan

## refresh

Update state with live values from providers without making any changes.

```sh
blue refresh --state blue.state.json
```

### Flags

| Flag | Description |
|---|---|
| `--providers` | Path to provider config file (default: `blue.providers.toml`) |
| `--state` | Path to state file (default: `blue.state.json`) |

### What it does

Calls `read` on every resource in state and updates their outputs with current values from the provider. Useful for:

- Detecting out-of-band changes
- Recovering from a crashed deploy
- Verifying state matches reality

If a resource no longer exists at the provider, it's removed from state with a warning. If a read fails (e.g. network error), the resource is left as-is in state.

::: tip
You don't usually need to run `refresh` manually — `plan` always runs a refresh as its first step. Use `refresh` on its own when you want to update state without computing a plan (e.g. to verify state matches reality after manual changes).
:::

## destroy

Delete all managed resources.

```sh
blue destroy --state blue.state.json
```

### Flags

| Flag | Description |
|---|---|
| `--providers` | Path to provider config file (default: `blue.providers.toml`) |
| `--state` | Path to state file (default: `blue.state.json`) |

### What it does

Reads the state file and deletes every resource in reverse dependency order. After each successful deletion, the resource is removed from state. If a deletion fails, destroy stops immediately.

No config file is needed — destroy works from state alone.
