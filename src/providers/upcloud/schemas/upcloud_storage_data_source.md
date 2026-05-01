# storage (data source)

Looks up an existing UpCloud storage volume — templates, normal disks, backups, CD-ROM images, or favorites — by filtering against the storage list.

All filters are AND'd. Exactly one storage must match; zero or multiple matches are an error. The data source runs fresh on every plan; results are not cached.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### Looking up a public template by title

The most common pattern — pin a server's storage to a specific OS template:

```toml
[data.ubuntu]
type = "upcloud.storage"
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }

[resources.web-01]
type = "upcloud.server"
storage = "{{ data.ubuntu.uuid }}"
```

### Looking up by uuid

When you already know the storage's UUID (e.g. recovered from another deploy or external tooling):

```toml
[data.existing]
type = "upcloud.storage"
filters = { uuid = "01287ad1-496c-4b5f-bb67-0fc2e3494740" }
```

### Refining filters when multiple match

If multiple storages match, the plan errors with the count. Add fields to narrow until exactly one matches:

```toml
filters = { type = "template", title = "Ubuntu Server 24.04 LTS", access = "public" }
```

The same applies to backups, normal disks, and CD-ROM images — UpCloud accounts can have many entries with similar titles, so include `type` (or `access`) when ambiguity is likely.

### Filtering across multiple instances

When more than one provider instance is configured, pick which account to look up against with the `provider` field:

```toml
[data.us_template]
type = "upcloud.storage"
provider = "upcloud-us"
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }
```
