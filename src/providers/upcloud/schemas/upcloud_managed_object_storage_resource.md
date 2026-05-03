# managed_object_storage (resource)

Manages a UpCloud Managed Object Storage service — an S3-compatible object store. The resource handles the full lifecycle: provisioning (which is asynchronous and may take several minutes to settle), in-place updates (rename, status flip, network attachments, labels), and deletion.

A new service is empty: the bucket, IAM user, policy, and access-key resources for it are not yet supported in Blue and must be managed out of band against the service's S3 / IAM / STS endpoints (surfaced in `endpoints` outputs).

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/object-storage-2`. Returns immediately with `operational_state: "pending"`; this resource then polls `GET /1.3/object-storage-2/{uuid}` every 10s until `operational_state` matches the target derived from `configured_status` (or 10 minutes elapse, whichever comes first). |
| **read** | `GET /1.3/object-storage-2/{uuid}`. Refreshes outputs from live state. |
| **update** | `PATCH /1.3/object-storage-2/{uuid}` for in-place changes (`name`, `configured_status`, `termination_protection`, `networks`, `labels`). The `region` field is force-new and triggers replacement instead. After PATCH, the same poll-until-target-state loop runs. |
| **delete** | `DELETE /1.3/object-storage-2/{uuid}` (with `?force=true` if `force_destroy = true`). |

## Asynchronous create

UpCloud provisions the service (allocates endpoints, issues TLS certificates, attaches networks) over the course of several minutes. The `endpoints` array is empty until the service reaches `operational_state: "running"` — downstream code that needs to talk to the S3 / IAM / STS URLs must wait for create to return. This resource handles that for you by polling.

If polling times out (default 10 minutes), the resource is left in state with the UUID it was assigned at create time. Subsequent `blue refresh` will read its current state; `blue deploy` against unchanged config will be a no-op once the service finishes settling.

## Termination protection

When `termination_protection = true`, UpCloud refuses both delete and stop. To destroy a service with protection on, set `termination_protection = false` and run `blue deploy` first; then `blue destroy` will succeed. This resource pre-flights the destroy: if the saved state has protection on, it errors out before calling the API. If you flipped protection off in the UpCloud control panel, run `blue refresh` first to update saved state.

## Force destroy

By default, UpCloud refuses to delete a service that still contains buckets, IAM users, or policies. Set `force_destroy = true` to pass `?force=true` on the DELETE call and have UpCloud tear down everything inside the service. Set this with care — once issued, the API will not let you recover the data.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### Minimal service

```toml
[resources.assets]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"
```

After deploy, <code v-pre>{{ resources.assets.endpoints }}</code> exposes the S3 / IAM / STS URLs the service is reachable on. `configured_status` defaults to `started` and `termination_protection` to `false`.

### With networks and labels

```toml
[resources.media]
type = "upcloud.managed_object_storage"
name = "media-prod"
region = "europe-1"
configured_status = "started"
termination_protection = true

[[resources.media.networks]]
name = "public-ipv4"
type = "public"
family = "IPv4"

[[resources.media.networks]]
name = "internal"
type = "private"
family = "IPv4"
uuid = "03aa7245-2ff9-49c8-9f0e-7ca0270d71a4"

[[resources.media.labels]]
key = "env"
value = "prod"

[[resources.media.labels]]
key = "team"
value = "platform"
```

A private network must reference an existing SDN private network UUID in the same region as the service. Public networks just need a `name` (used as the label inside the service) and `family = "IPv4"`.

### Stopping a service without destroying it

```toml
[resources.assets]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"
configured_status = "stopped"
```

Switching `configured_status` from `started` to `stopped` (or vice versa) issues a PATCH and then polls until `operational_state` matches the new target. Buckets, users, and policies are preserved across stop/start.

### Allowing destroy of a non-empty service

```toml
[resources.scratch]
type = "upcloud.managed_object_storage"
name = "scratch"
region = "europe-1"
force_destroy = true
```

With `force_destroy = true`, `blue destroy` will tear down the service even if it still contains buckets and IAM resources. Default is `false` - UpCloud's default behavior, which requires you to empty the service first.
