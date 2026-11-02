# VPN tunnels for agent groups

Status: proposed, 2026-11-01. Only the assignment policy is built, and it
is not wired into the run. Nothing here is measured.

An operator can route agents' traffic through WireGuard tunnels, for
privacy or to reach a customer's network. Panoptes spreads agents across
every tunnel it is given.

## What the operator gives

One setting in the loop's config:

```json
"vpn": "/etc/panoptes/vpn/"
```

A directory of WireGuard config files, as the VPN provider hands them out,
one per tunnel. The file name is the tunnel's name: `us.conf` is tunnel
`us`. Any provider that hands out WireGuard configs works, so there is no
code per provider. With no `vpn` setting, agents start outside any tunnel,
as today.

The policy also supports a limit per tunnel and a pin per task, so a task
runs only on one tunnel. Neither is a setting yet: every tunnel has no
limit, and no task is pinned. They are there for an API to set later.

## How it works

Each tunnel is a network namespace, `/run/netns/vpn-NAME`, holding one
WireGuard interface. The interface is created in the host namespace and
then moved in, as WireGuard's
[namespace guide](https://www.wireguard.com/netns/) describes: its UDP
socket stays on the host's network, and the namespace sees only the tunnel.
The namespace gets its own `/etc/netns/vpn-NAME/resolv.conf`, so DNS goes
through the tunnel too.

An agent joins a tunnel through its desk's unit:
`systemd-run -p NetworkNamespacePath=/run/netns/vpn-NAME ...`. The loop
fills `{netns}` in the `launch` template.

The loop reaches the desk, and the desk reaches resd, over Unix sockets.
A Unix socket is a file path and does not depend on the network namespace,
so moving an agent into a tunnel changes none of Panoptes' own wiring.

## Assignment

A pure policy, keyed by integer ids, as
[verification.md](verification.md) asks. It chooses a tunnel when an agent
starts a task, and never moves an agent during a task.

1. A pinned task gets its tunnel, or waits.
2. Agents signed in to the same account share a tunnel. One account seen
   from several countries at once is the quickest way to get it locked.
3. Otherwise, the up tunnel with the fewest agents, below its limit.
4. No tunnel up or below its limit: the agent waits, frozen or parked.

## Failure

A tunnel that goes down takes no new agents. Its agents have no route
anywhere else, so they lose the network instead of falling back to the
host's address. The loop parks them, and they resume when the tunnel is
back.

## Checks

| Layer | Check |
|---|---|
| policy | `crates/panoptes-loop/src/tunnel.rs`: 16 example tests, plus every sequence of up to 5 inputs over 2 tunnels, 3 agents and 1 account, with limits of 1 or none: starts, ends, pins, shared accounts, tunnels going down and up. About 2 s in a debug build |
| I/O | a NixOS VM check with a WireGuard peer inside the VM standing in for the provider |

The VM check proves three things:

- An agent in tunnel `a` reaches a test server with tunnel `a`'s address.
- An agent in tunnel `a` cannot reach tunnel `b`'s namespace.
- With tunnel `a` down, an agent in it reaches nothing, and the host's
  address never shows at the test server.

## Not in the first version

- Adding, swapping or removing tunnels while the loop runs.
- Tunnel bandwidth: every agent on a tunnel shares it. Browsing is light,
  0.7 to 1.7 MB a page load (finding 9), so this is not measured yet.
- Rotating addresses. The design pins an agent to one tunnel for a whole
  task on purpose.

## Questions for review

- Does this belong in panoptes-loop while the loop still drives the
  earlier desk, or should it wait for the loop's port to the Wayland desk?
- Is a unit property the right place, or should resd own the namespaces,
  since it already owns each agent's cgroup?
