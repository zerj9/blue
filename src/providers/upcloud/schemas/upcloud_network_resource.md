# network (resource)

Manages a UpCloud SDN private network. Cloud servers in the same zone can be attached to the network for east-west traffic; routers and network peerings give it reach beyond a single network.

This resource manages the network itself — defining its address space, DHCP settings, and optional router attachment. Attaching/detaching servers is handled by the server resources (or out-of-band).

## Lifecycle

| Operation | Behavior |
|---|---|
| **create** | `POST /1.3/network`. Synchronous — returns the network's full state in one round-trip. No provisioning wait. |
| **read** | `GET /1.3/network/{uuid}`. Refreshes outputs from live state. |
| **update** | `PUT /1.3/network/{uuid}` for in-place changes (`name`, `router`, `ip_networks`, `labels`). The `zone` field is force-new and triggers replacement instead. |
| **delete** | `DELETE /1.3/network/{uuid}`. Fails with `409 NETWORK_NOT_EMPTY` if servers are still attached — detach them first. UpCloud has no force flag for this. |

## Router attachment

Set `router = "<uuid>"` to attach this network to an existing router. Omit (or set to empty string) to leave it unattached. Changes to `router` after creation are honored in-place: the network is detached or re-attached without replacement.

This resource always sends `router` (null when omitted) on both create and update so removing the field from config actually detaches rather than leaving the prior attachment in place.

## DHCP routes auto-population

`ip_networks[].dhcp_routes_configuration.effective_routes_auto_population` controls whether UpCloud automatically advertises router-connected-network and static routes via DHCP. Network peering routes are auto-populated regardless of this setting.

The `dhcp_effective_routes` field that UpCloud computes per ip_network entry is **not** exposed in outputs — it's read-only, server-computed, and very chatty (it includes every peering, router-connected, and static route). Use the UpCloud control panel or API directly if you need to inspect it.

## Inputs

<!-- @auto:inputs -->

## Outputs

<!-- @auto:outputs -->

## Examples

### Minimal private network

```toml
[resources.app_net]
type = "upcloud.network"
name = "app-net"
zone = "uk-lon1"

[[resources.app_net.ip_networks]]
address = "172.16.0.0/22"
```

Creates a `/22` SDN private network with DHCP enabled (default). The first usable address is assigned as gateway.

### Attached to a router with labels

```toml
[resources.prod_net]
type = "upcloud.network"
name = "prod"
zone = "uk-lon1"
router = "04c0df35-2658-4b0c-8ac7-962090f4e92a"

[[resources.prod_net.ip_networks]]
address = "172.16.0.0/22"
dhcp = "yes"
dhcp_default_route = "no"
gateway = "172.16.0.1"
dhcp_dns = ["172.16.0.10", "172.16.1.10"]

[[resources.prod_net.labels]]
key = "env"
value = "prod"
```

### With DHCP route auto-population filters

```toml
[resources.filtered_net]
type = "upcloud.network"
name = "filtered"
zone = "uk-lon1"

[[resources.filtered_net.ip_networks]]
address = "10.20.0.0/24"
dhcp_routes = ["192.168.0.0/24-nexthop=10.20.0.100"]

[resources.filtered_net.ip_networks.dhcp_routes_configuration.effective_routes_auto_population]
enabled = "yes"
exclude_by_source = ["static-route"]
filter_by_route_type = ["user"]
```

`exclude_by_source` entries must be `router-connected-networks` or `static-route`. `filter_by_route_type` entries must be `user` or `service`. These are validated at plan time.
