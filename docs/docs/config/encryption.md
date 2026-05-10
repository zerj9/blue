# Encryption

Blue encrypts resource outputs flagged `secret = true` in their schema before writing them to the state file. Encryption uses [age](https://age-encryption.org/) — a modern, audited file-encryption format — and supports multiple recipients so a team can share the same encrypted state.

## What gets encrypted (and what doesn't)

Encrypted at rest in `blue.state.json`:

- Resource **outputs** whose schema marks them `secret = true` (for example, `secret_access_key` on `upcloud.managed_object_storage_user_access_key`).

Not encrypted:

- Resource **inputs**, even ones that contain sensitive values like API tokens. (A future change can extend the schema to mark input fields secret too; this version covers outputs only.)
- Parameters, data source results, and the rest of the state file.

In memory, after `read_state` runs, every value is plaintext — providers, the diff engine, and template interpolation always see decrypted values. Encryption is purely a disk boundary.

## Configuring recipients

Recipients are age public keys that can decrypt your state. List them in your resource config:

```toml
[encryption]
recipients = [
  "age1qzlk2v...your-pubkey-here...",
  "age1xy4t9d...teammate-pubkey...",
]
```

| Field | Type | Required | Description |
|---|---|---|---|
| `recipients` | array of strings | yes if any resource type has `secret = true` outputs | age public keys that can decrypt secret outputs |

Each recipient must be a valid X25519 age public key (`age1...` format). Typos fail at config-parse time with a clear error.

When you use any resource type whose schema has `secret = true` outputs and `[encryption]` is missing or has an empty `recipients` list, `blue plan` refuses with:

```
resource '<name>' (type '<type>') has secret outputs but no [encryption] recipients are configured;
add a [encryption] block with at least one age recipient before planning
```

## Generating an age identity

```bash
age-keygen -o ~/.config/blue/identity.txt
```

This writes a private identity (`AGE-SECRET-KEY-1...`) to the file and prints the matching public key (`age1...`) to stdout. Add the public key to your config's `[encryption].recipients`. The private file stays on the machine that needs to read state.

## Loading your identity

Blue reads identities from environment variables only — no default file path is consulted, to avoid surprise file reads.

| Variable | Form | Purpose |
|---|---|---|
| `BLUE_AGE_IDENTITY` | path to a file (one or more identities, age-keygen format) | typical use |
| `BLUE_AGE_IDENTITY_KEY` | literal `AGE-SECRET-KEY-1...` string | CI / secret-manager workflows where the key is injected without touching disk |

Both can be set; identities from both sources are concatenated. `age` tries each identity until one decrypts.

If any encrypted markers exist in your state file and no identity is loaded, every command that reads state fails with:

```
state file '<path>' contains encrypted values but no identity is configured
(set BLUE_AGE_IDENTITY or BLUE_AGE_IDENTITY_KEY)
```

## Adding or removing recipients — `blue rekey`

age recipients are write-time only: a ciphertext can only be decrypted by an identity whose recipient was listed when the value was encrypted. Adding a new recipient to your config does **not** retroactively grant access to existing state values; you must re-encrypt with the new recipient set.

Blue's deploy path detects this drift and refuses to proceed:

```
recipient set has changed since last write
  state was last written with: ["age1aaa", "age1bbb"]
  config now has:              ["age1aaa", "age1bbb", "age1ccc"]
run `blue rekey` to re-encrypt state with the new recipient set
```

To resolve, run:

```bash
blue rekey -f config.toml --state blue.state.json
```

This:

1. Loads the existing state, decrypting markers under your current identity.
2. Re-encrypts each secret output under the recipient set in your config.
3. Updates `state.encrypted_with` (the recipient list recorded inside state) to match.

Caveats:

- Removing a recipient via rekey only revokes access to **future** ciphertext. Anyone who already had a current identity may have copies of decrypted values; if you're treating their access as compromised, also rotate the underlying secret (e.g. delete and recreate the access key).
- You must hold an identity that can still decrypt the current state. If the only person with an identity leaves, state is unreadable — same caveat as SOPS / any age-based system.
- Rekey is the only command that intentionally changes recipients. `blue refresh` and `blue destroy` re-encrypt with whatever was last written (recorded in `state.encrypted_with`), so they don't accidentally rotate keys.

## At-rest format

Each encrypted value is written as a single string in this shape:

```
<blue:enc:v1:HMAC_HEX:BASE64_AGE_CIPHERTEXT>
```

- `v1` is the format version. Future format changes will introduce `v2` etc.; old versions stay parseable by the current code.
- `HMAC_HEX` is HMAC-SHA256 of the plaintext keyed by `<resource_name>.<field_path>`. This is a **fingerprint**, not a confidentiality boundary — equal plaintexts at the same field produce equal HMACs. Its purpose is to let the diff engine detect plaintext changes without decrypting.
- `BASE64_AGE_CIPHERTEXT` is a binary age v1 ciphertext, base64-encoded so it fits inside a JSON string.

The state file remains valid JSON; only secret values become opaque. `git diff` still works for non-secret fields.

The state file also gains a top-level `encrypted_with` field, listing the sorted recipient strings used at the most recent write. Recipients are public keys, not secrets — storing them in cleartext is fine and is what powers the drift-detection check above.

## Limitations

- **Inputs are not encrypted.** Input fields containing sensitive values land in state plaintext. Treat your state file as sensitive overall and protect it accordingly (file permissions, gitignore, remote backend in the future).
- **HMAC fingerprint is not a MAC.** It uses a public salt (`<resource>.<field>`) so anyone with the state file can compute candidate fingerprints. It only guards against silent value changes, not tampering.
- **Re-encryption produces fresh ciphertext.** age uses a random ephemeral key per encryption, so saving an unchanged secret produces a different base64 payload (the HMAC stays stable). Expect state file diffs on every save — this is benign for git but noisy.
- **Rotating a recipient doesn't invalidate prior access.** `blue rekey` re-encrypts state, but the old recipient's identity could already have decrypted the values. Rotate the underlying secret if access compromise is a concern.
