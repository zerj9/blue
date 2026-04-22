# State

Blue tracks deployed resources in a state file. The state file is the source of truth for what Blue has created — it stores resource outputs, inputs, and dependency information.

## File location

Default: `blue.state.json` in the working directory. Override with the `--state` flag:

```sh
blue plan -f config.toml --state path/to/state.json
```

## Format

```json
{
  "lineage": "uuid-generated-on-first-create",
  "serial": 42,
  "resources": {
    "web-01": {
      "type": "upcloud.server",
      "inputs": { "hostname": "dev-env", "zone": "uk-lon1" },
      "outputs": { "uuid": "abc-123", "state": "started" },
      "depends_on": ["resources.object-store"]
    }
  }
}
```

| Field | Description |
|---|---|
| `lineage` | UUID generated when the state file is first created. Never changes. |
| `serial` | Incremented on every write. Used with lineage for staleness detection. |
| `resources` | Map of resource name to resource state |

Each resource in state contains:

| Field | Description |
|---|---|
| `type` | The resource type (e.g. `upcloud.server`) |
| `inputs` | Resolved input values that were deployed. Used for diffing on next plan. |
| `outputs` | Values returned by the provider (e.g. UUID, IP address). Used by `read` and referenced by other resources. |
| `depends_on` | Dependency names, used for ordering during refresh and destroy. |

## What's not in state

- **Parameters** — resolved fresh every plan
- **Data sources** — run fresh every plan, no state needed
- **Unresolved references** — all values in state are fully resolved, no `{{ }}` templates

## Staleness detection

When `plan` produces a plan artifact, it records the current lineage and serial. When `deploy` runs, it checks these match the current state file. If someone ran another deploy or refresh in between, the serial won't match and deploy refuses to run. Re-run `plan` to get a fresh plan.

## Recovery

Blue saves resource outputs to state during and after operations. If Blue crashes or an operation fails:

- **Next plan** runs `refresh` first, calling `read` on every resource to check what actually exists at the provider
- Resources that exist are updated with live values
- Resources that no longer exist are removed from state
- The plan then diffs the refreshed state against your config

You don't need to manually fix the state file — `plan` handles reconciliation automatically.

## Deleting resources

When you remove a resource from config, Blue detects it's in state but not in config and plans a **delete** operation. Deletion order is determined by the `depends_on` field in state — dependents are deleted before their dependencies.

If a deleted resource's dependency has already been removed from both config and state, the dependency edge is ignored and the resource is deleted normally.

## Known limitations

1. **External changes between plan and deploy.** Deploy trusts the plan and does not read live state. If a resource is changed out-of-band between plan and deploy, deploy may fail. The fix is always to re-run `plan`.

2. **Data sources always re-run.** No caching. If a data source calls an expensive external API, every plan incurs that cost.

3. **Script resource replacement may cascade unnecessarily.** When a script resource's `triggers_replace` values change, the replacement cascades to dependent resources even if the script output happens to be the same. This is by design — `triggers_replace` means you opted into the cascade.
