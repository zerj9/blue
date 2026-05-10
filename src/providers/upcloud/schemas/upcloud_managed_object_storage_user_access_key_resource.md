# managed_object_storage_user_access_key (resource)

Manages a single access key (an `(access_key_id, secret_access_key)` pair) for a service user inside a `upcloud.managed_object_storage` service. Use the resulting key to authenticate S3-compatible clients against the service's S3 endpoints.

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/object-storage-2/{service_uuid}/users/{username}/access-keys`. Synchronous — returns 201 with the new key. **The `secret_access_key` is returned only on this call**; UpCloud never returns it again. Blue stores it (encrypted) in state at this point. |
| **read** | `GET /1.3/object-storage-2/{service_uuid}/users/{username}/access-keys/{access-key-id}`. The response omits `secret_access_key`; Blue carries the previously stored value forward in state via the `preserve_secret_outputs` mechanism. |
| **update** | `PATCH .../{access-key-id}` with `{ "status": "..." }`. Only `status` is modifiable; both `service_uuid` and `username` are `force_new`. |
| **delete** | `DELETE .../{access-key-id}`. 404 is treated as already-gone (idempotent). |

## The secret is single-shot

UpCloud's API gives you `secret_access_key` exactly once, at create time. Blue captures it and stores it in state encrypted under the `[encryption].recipients` you've configured (see [Encryption](../../config/encryption.md)). Subsequent `blue refresh` calls observe that the API stops returning the secret and explicitly preserve the previously stored value.

If you lose the encrypted state file (or the identity needed to decrypt it), the secret is unrecoverable — you must destroy and recreate the access key.

## Encryption is required

Because `secret_access_key` is flagged `secret = true` in the schema, `blue plan` refuses to plan unless your config has an `[encryption]` block with at least one recipient. This is a hard precondition — the secret would otherwise land in state cleartext.

## Replace destroys the secret

Changing `service_uuid` or `username` triggers a Replace (delete + create). The original key is deleted at the provider, a new one is minted, and **the new `secret_access_key` is different from the old one**. Any S3-compatible client configured with the old secret will start failing as soon as the delete completes. Plan rotations during a maintenance window.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### A user with an access key

```toml
[encryption]
recipients = ["age1qzlk2v...your-pubkey..."]

[resources.assets_service]
type = "upcloud.managed_object_storage"
name = "assets"
region = "europe-1"
force_destroy = true

[resources.app_user]
type = "upcloud.managed_object_storage_user"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "app_writer"

[resources.app_key]
type = "upcloud.managed_object_storage_user_access_key"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
```

After deploy, the access key is created and stored encrypted in state. Reference it from another resource via <code v-pre>{{ resources.app_key.access_key_id }}</code> and <code v-pre>{{ resources.app_key.secret_access_key }}</code>.

### Disabling a key without destroying it

```toml
[resources.app_key]
type = "upcloud.managed_object_storage_user_access_key"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
status = "Inactive"
```

`status = "Inactive"` rejects S3 requests but keeps the key intact and re-activatable. Switching back to `"Active"` restores access without rotating the secret.

### Multiple keys per user

UpCloud allows multiple access keys per service user (typical IAM pattern, useful for rotation):

```toml
[resources.app_key_a]
type = "upcloud.managed_object_storage_user_access_key"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"

[resources.app_key_b]
type = "upcloud.managed_object_storage_user_access_key"
service_uuid = "{{ resources.assets_service.uuid }}"
username = "{{ resources.app_user.username }}"
```

Each Blue resource declaration produces an independent key; `access_key_id` and `secret_access_key` differ between them. Useful for zero-downtime rotation: deploy a new key, switch clients to it, then remove the old declaration.
