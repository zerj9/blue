# managed_object_storage_user (resource)

Manages a single service user (IAM identity) inside a `upcloud.managed_object_storage` service. Users are the principals that hold access keys and policies for talking to the service's S3, IAM, and STS endpoints.

This resource only covers the user identity itself: create, read, delete. Access keys, IAM policies, and policy attachments are managed by separate UpCloud APIs and are not exposed here — manage them via the UpCloud control panel or, in the future, dedicated Blue resources.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/object-storage-2/{service_uuid}/users`. Synchronous — returns 201 with the new user as soon as it exists. |
| **read** | `GET /1.3/object-storage-2/{service_uuid}/users/{username}`. Direct lookup — no pagination needed. |
| **update** | Not supported — both inputs (`service_uuid`, `username`) are `force_new`. Any change to either produces a Replace (delete + create), which permanently deletes the original user (and detaches its access keys / policies). |
| **delete** | `DELETE /1.3/object-storage-2/{service_uuid}/users/{username}`. Returns 204. The resource then polls until GET returns 404 so an immediate same-name recreate (Replace flows where a user is deleted then recreated) doesn't race async cleanup. |

## Replace is destructive

Changing `username` triggers a Replace: the old user is deleted, then a new one is created. Any access keys attached to the old user become invalid (the keys are tied to the user identity), and any out-of-band policies must be re-attached. Plan accordingly.

## Parent service ordering

`service_uuid` is wired from the parent service's output, e.g. <code v-pre>{{ resources.my_service.uuid }}</code>. Blue's dependency graph uses that reference to ensure the parent service is created before the user, and destroyed *after* it. If the parent service is deleted out of band, this resource's `read` call sees a 404 and reports `NotFound` — Blue then drops the user from state on the next refresh.

## Reserved username

`_upcloud-internal-user` is reserved by UpCloud for its own use and is rejected at plan time, before any API call is made.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### A user inside a service

```toml
[resources.assets_service]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"
force_destroy = true

[resources.app_user]
type = "upcloud.managed_object_storage_user"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "app_writer"
```

The user is created once the service reports `operational_state: "running"`. After deploy, attach IAM policies and generate access keys via the UpCloud control panel — those operations aren't yet covered by Blue.

### Multiple users under one service

```toml
[resources.media_service]
type = "upcloud.managed_object_storage"
name = "media"
region = "europe-1"

[resources.uploader]
type = "upcloud.managed_object_storage_user"
service_uuid = "{{ resources.media_service.uuid }}"
username = "uploader"

[resources.reader]
type = "upcloud.managed_object_storage_user"
service_uuid = "{{ resources.media_service.uuid }}"
username = "reader"
```

Users are independent — destroying one does not affect the others.
