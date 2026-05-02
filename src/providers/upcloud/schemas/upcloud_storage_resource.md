# storage (resource)

Manages a UpCloud storage volume — a block-level disk that can be attached to servers in the same zone. The resource handles the full lifecycle: provisioning, in-place updates (resize, rename, backup schedule, labels), and deletion.

A newly created storage is detached from any server. Attaching is done via the `upcloud.server` resource that references this storage's `uuid`.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/storage`. Returns once UpCloud has provisioned the volume (typically state `online`). |
| **read** | `GET /1.3/storage/{uuid}`. Refreshes outputs from live state. |
| **update** | `PUT /1.3/storage/{uuid}` for in-place changes (`title`, `size`, `backup_rule`, `labels`). Force-new fields (`zone`, `tier`, `encrypted`) trigger replacement instead. |
| **delete** | `DELETE /1.3/storage/{uuid}`. |

## Resizing

`size` is updatable in place, but UpCloud requires the storage to be **detached from any server** before resize. If the storage is attached at update time, the API returns `409 STORAGE_ATTACHED` and the plan fails with that error message. Detach the storage manually (or via the server resource that holds it), apply the resize, then reattach.

The new `size` must be greater than the current size — UpCloud doesn't support shrinking. Filesystem and partition table changes are not made automatically; once the volume grows, resize the filesystem from inside the attached server.

## Encryption

`encrypted` is set at creation only. Changing it later forces a replace (delete + recreate), which destroys the data. Plan for this from the start.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### Basic volume

```toml
[resources.data]
type = "upcloud.storage"
title = "data-disk"
size = 100
zone = "uk-lon1"
tier = "maxiops"
```

After deploy, <code v-pre>{{ resources.data.uuid }}</code> references the volume's UUID — a server resource in the same zone can mount it.

### Encrypted at rest

```toml
[resources.secret_disk]
type = "upcloud.storage"
title = "secret-data"
size = 50
zone = "uk-lon1"
tier = "maxiops"
encrypted = "yes"
```

`encrypted` is `force_new` — once set, changing it triggers a replace (the volume is destroyed and a new one is created).

### With a daily backup schedule

```toml
[resources.db_disk]
type = "upcloud.storage"
title = "production-database"
size = 200
zone = "fi-hel1"
tier = "maxiops"

[resources.db_disk.backup_rule]
interval = "daily"
time = "0430"
retention = 14
```

Backups run every day at 04:30 UTC and are kept for 14 days. To take backups only on a specific weekday, use `interval = "mon"` (or `tue`, `wed`, etc.). All three fields (`interval`, `time`, `retention`) must be specified together.

### With labels

```toml
[resources.tagged_disk]
type = "upcloud.storage"
title = "tagged-disk"
size = 20
zone = "uk-lon1"

[[resources.tagged_disk.labels]]
key = "env"
value = "prod"

[[resources.tagged_disk.labels]]
key = "team"
value = "platform"
```

Labels are filterable from the storage data source and visible in the UpCloud control panel. Keys must be 2-32 printable ASCII characters and must not start with `_` (underscore-prefixed keys are reserved for service labels).

### Tier selection

UpCloud offers three tiers:

- `maxiops` — NVMe-backed, fastest, default for production workloads.
- `standard` — balanced cost/performance.
- `hdd` — slower spinning disks, cheapest. The default if `tier` is omitted.

Tier cannot be changed in place — switching tiers requires creating a new storage and copying data manually (or via clone, not yet supported here).
