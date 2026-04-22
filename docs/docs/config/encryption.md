# Encryption

Blue encrypts secret values when writing state and plan files to disk.

## Configuration

```toml
[encryption]
recipients = [
  "age1abc..."
]
```

| Field | Type | Required | Description |
|---|---|---|---|
| `recipients` | array of strings | yes (if secrets are used) | age public keys that can decrypt the secrets |

If any parameter has `secret = true` or any resource schema marks outputs as `secret = true`, the `[encryption]` section with at least one recipient is required.

## How it works

- Fields and outputs marked `secret = true` in the resource schema are encrypted using [age](https://age-encryption.org/) when writing to disk.
- When reading state or plan files back, they are decrypted.
- Diffing and deploy logic always work with plaintext in memory.
- HMAC stability ensures that re-encrypting an unchanged secret doesn't cause a false diff.

## Secret fields in templates

Secrets can be interpolated into other fields using templates:

::: v-pre
```toml
[parameters.github_token]
secret = true
env = "GITHUB_TOKEN"

[resources.web-01.inputs]
user_data = "#!/bin/bash\necho '{{ parameters.github_token }}'"
```
:::

The `user_data` field contains the plaintext secret in memory for diffing and deployment. It is encrypted when written to the state file.

::: warning
Once a secret is interpolated into another field, that field contains the plaintext value in memory during plan and deploy. Encryption only protects data at rest (state and plan files on disk).
:::
