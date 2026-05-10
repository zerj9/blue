# managed_object_storage_user_policy (resource)

Attaches a single IAM policy to a `upcloud.managed_object_storage_user`. Policies are pre-existing built-ins owned by UpCloud (e.g. `ECSS3FullAccess`, `IAMFullAccess`) — this resource manages the *attachment*, not the policy itself.

One attachment per resource. To grant a user multiple policies, declare multiple attachment resources.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/object-storage-2/{service_uuid}/users/{username}/policies` with `{"name": policy_name}`. Synchronous. The POST returns no body, so Blue follows up with a LIST (`GET .../policies`) to populate `arn`. |
| **read** | `GET /1.3/object-storage-2/{service_uuid}/users/{username}/policies` (LIST). Blue scans the array for the entry whose `name` matches `policy_name`. Absent → resource gone. |
| **update** | Not supported — every input is `force_new`. Any change produces a Replace (detach + re-attach). |
| **delete** | `DELETE /1.3/object-storage-2/{service_uuid}/users/{username}/policies/{policy_name}`. 404 is treated as already-gone (idempotent). |

## Attachments, not policies

This resource does not create or modify the underlying IAM policy. It only records that policy `X` is attached to user `Y` in service `Z`. The policy must already exist in the service; if you reference one that doesn't, the POST fails with an error from UpCloud.

## Replace is destructive (briefly)

Changing `service_uuid`, `username`, or `policy_name` triggers a Replace: detach + reattach. Between the two calls the user has no permissions from this attachment. Plan rotations during a maintenance window if the user is actively serving traffic.

## Parent ordering

`service_uuid` and `username` are typically wired from parent resources, e.g. <code v-pre>{{ resources.my_user.username }}</code>. Blue's dependency graph uses those references to ensure parents are created first and destroyed last. If the user or service is deleted out of band, this resource's `read` sees a 404 (or the listing comes back without our entry) and reports `NotFound`; Blue drops the attachment from state on the next refresh.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### Attaching a single policy

```toml
[resources.assets_service]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"

[resources.app_user]
type = "upcloud.managed_object_storage_user"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "app_writer"

[resources.app_user_s3_full]
type = "upcloud.managed_object_storage_user_policy"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
policy_name = "ECSS3FullAccess"
```

### Multiple policies on one user

UpCloud allows multiple policies per user. Declare each attachment separately:

```toml
[resources.app_user_s3_full]
type = "upcloud.managed_object_storage_user_policy"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
policy_name = "ECSS3FullAccess"

[resources.app_user_iam_full]
type = "upcloud.managed_object_storage_user_policy"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
policy_name = "IAMFullAccess"
```

Removing one declaration detaches just that policy; the other stays.
