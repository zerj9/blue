# managed_object_storage_bucket (resource)

Manages a single bucket inside a `upcloud.managed_object_storage` service. The bucket holds objects accessible via the parent service's S3 endpoints.

This resource is intentionally minimal — it covers only what UpCloud's API exposes for buckets directly: create by name, delete, and list-with-metrics. Object-level operations, bucket policies, lifecycle rules, versioning, CORS, and static-website settings are not exposed through this API and must be configured out of band against the S3 endpoint using an S3-compatible client.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/object-storage-2/{service_uuid}/buckets`. Synchronous — returns a 201 with the new bucket as soon as it exists. |
| **read** | `GET /1.3/object-storage-2/{service_uuid}/buckets` (paginated, page size 100), filtered client-side by name. There is no GET-by-name endpoint, so refresh has to walk the list. |
| **update** | Not supported — both inputs (`service_uuid`, `name`) are `force_new`. Any change to either produces a Replace (delete + create), which is destructive. |
| **delete** | `DELETE /1.3/object-storage-2/{service_uuid}/buckets/{name}`. **Permanently removes the bucket and every object it contains.** UpCloud's API has no opt-out: there is no "refuse if non-empty" mode and no `force` flag. The deletion cannot be reversed. |

## Delete is destructive — always

There is no `force_destroy` input on this resource. The DELETE call wipes the bucket and all of its objects unconditionally; we don't add a flag because there's no API behavior the flag could opt into. Plan accordingly: a `blue destroy` (or any change to `name` / `service_uuid`, both of which trigger Replace) will erase data without confirmation. If you need stronger guards, gate the destroy at your workflow level (require approval before running `blue destroy` against production state).

## Parent service ordering

`service_uuid` is wired from the parent service's output, e.g. <code v-pre>{{ resources.my_service.uuid }}</code>. Blue's dependency graph uses that reference to ensure the parent service is created before the bucket. The parent `upcloud.managed_object_storage` resource also polls until the service reaches `operational_state: "running"` before its create returns, so by the time the bucket POST fires the service is fully up — no extra wait is needed here.

If the parent service is destroyed (or its `service_uuid` ever changed in some pathological way), the bucket's `read` call sees a 404 on the list endpoint and reports `NotFound` — Blue then drops the bucket from state on the next refresh.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### A bucket inside a service

```toml
[resources.assets_service]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"
force_destroy = true

[resources.uploads]
type = "upcloud.managed_object_storage_bucket"
service_uuid = "{{ resources.assets_service.uuid }}"
name = "uploads"
```

The bucket is created once the service reports `operational_state: "running"`. After deploy, point any S3-compatible client at the service's `endpoints` outputs, authenticate with an IAM user/access key managed via the UpCloud control panel (IAM resources are not yet covered by Blue), and use `uploads` as the bucket name.

### Multiple buckets under one service

```toml
[resources.media_service]
type = "upcloud.managed_object_storage"
name = "media"
region = "europe-1"

[resources.images]
type = "upcloud.managed_object_storage_bucket"
service_uuid = "{{ resources.media_service.uuid }}"
name = "images"

[resources.videos]
type = "upcloud.managed_object_storage_bucket"
service_uuid = "{{ resources.media_service.uuid }}"
name = "videos"

[resources.thumbnails]
type = "upcloud.managed_object_storage_bucket"
service_uuid = "{{ resources.media_service.uuid }}"
name = "thumbnails"
```

Each bucket is independent — destroying one does not affect the others, and they can be added or removed across deploys.
