---
outline: deep
---

# Templates

Templates are references to values from parameters, data sources, and resources. They use the <code v-pre>{{ }}</code> syntax inside TOML values.

## Syntax

::: v-pre
```
{{ source.name.path }}
```
:::

| Part | Description |
|---|---|
| `source` | One of `parameters`, `data`, or `resources` |
| `name` | The name of the parameter, data source, or resource |
| `path` | Dot-separated path to the value in the outputs |

### Examples

::: v-pre
```toml
# Parameter reference
hostname = "{{ parameters.hostname }}"

# Data source output
storage = "{{ data.ubuntu.uuid }}"

# Resource output
server_uuid = "{{ resources.web-01.uuid }}"
```
:::

## Nested access

Use dot-path to traverse nested objects:

::: v-pre
```toml
domain = "{{ resources.object-store.config.endpoint.url }}"
```
:::

## Array indexing

Access array elements by numeric index:

::: v-pre
```toml
first_endpoint = "{{ resources.object-store.endpoints.0.domain_name }}"
```
:::

## Array filters

Filter array elements by field values. The filter must match exactly one element — Blue errors if zero or multiple elements match.

**Single filter:**

::: v-pre
```toml
public_endpoint = "{{ resources.object-store.endpoints[type=public].domain_name }}"
```
:::

**Multiple filters (AND):**

::: v-pre
```toml
ipv4_public = "{{ resources.object-store.endpoints[type=public,family=IPv4].domain_name }}"
```
:::

**Chained filters and indexing:**

::: v-pre
```toml
address = "{{ resources.server.networks[type=public].addresses[family=IPv4].address }}"
```
:::

## Type preservation

When a template reference is the **entire value**, the original type is preserved:

::: v-pre
```toml
# disk_size is an integer parameter with default = 10
size = "{{ parameters.disk_size }}"    # resolves to integer 10, not string "10"
```
:::

When a reference is **embedded in a larger string**, everything is stringified:

::: v-pre
```toml
title = "server-{{ parameters.name }}"  # always a string
```
:::

## What can be referenced

| Source | Accesses |
|---|---|
| `parameters` | The resolved parameter value |
| `data` | Outputs from the data source's `read` |
| `resources` | Outputs returned by the provider (not inputs) |

## Validation

- Every <code v-pre>{{ }}</code> reference must point to a declared dependency. Blue validates this when building the dependency graph.
- References that don't correspond to a dependency edge are a validation error — even if the value exists in the output map from a previous node.
- Array filters that match zero or multiple elements are an error at resolution time.
