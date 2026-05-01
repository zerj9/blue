# script (data source)

Runs a user-defined script and returns its output. Runs fresh on every plan — no state, no caching.

## Usage

```toml
[data.vault_creds]
type = "blue.script"
script = "scripts/fetch_creds.js"
inputs = { path = "secret/upcloud" }
```

## Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `script` | string | yes | Path to the script file, relative to the config file |
| `inputs` | table | no | Key-value pairs passed to the script as the `inputs` variable |

## Script format

Scripts run in a shipped Deno runtime. The script body is wrapped in a function with `inputs` available as a parameter. Use `return` to produce output:

```js
// scripts/fetch_creds.js
const response = await fetch(`https://vault.example.com/v1/${inputs.path}`);
const data = await response.json();
return {
  username: data.username,
  password: data.password,
};
```

Early returns are supported. Stderr is used for logging.

## Outputs

The returned object becomes the data source's outputs, accessible via templates:

::: v-pre
```toml
[resources.web-01]
type = "upcloud.server"
password = "{{ data.vault_creds.password }}"
```
:::
