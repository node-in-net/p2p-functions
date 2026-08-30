# Security Policy

## Reporting a vulnerability

Please report security issues privately, **not** through a public issue.

Use GitHub's private vulnerability reporting: open the repository's
**Security** tab and press **Report a vulnerability**, or go straight to
[the form](https://github.com/node-in-net/p2p-functions/security/advisories/new).
The report is visible only to the maintainers, and the fix can be developed in
a private fork from the same place.

Include what you can — the affected crate and version, what an attacker gains,
and steps or a proof of concept. We aim to acknowledge within 72 hours and to
keep you informed while a fix is prepared. Please give us a reasonable window to
release before disclosing publicly.

## What is in scope

This repository is where a peer's request turns into real work on the machine —
files are read, processes spawned, connections opened. That makes it the sharp
edge of the stack:

| Area | Where |
| --- | --- |
| Path sandboxing for shared folders | `node-functions/src/fs_local.rs` |
| SOCKS and HTTP relaying on a peer's behalf | `node-functions/src/socks5.rs`, `web_proxy.rs` |
| Terminal sessions | `node-functions/src/terminal.rs` |
| Windows registry reads and writes | `node-functions/src/registry.rs` |
| Screen capture and input injection | `node-functions/src/*_desktop.rs`, `mouse.rs` |
| Mapping messages to capabilities | `p2p-handlers/src/lib.rs` |
| Exposing a peer resource as a WebDAV mount | `web-davserver` |

Findings we consider serious include: escaping a shared folder's root, reaching
a capability the node did not enable, using a node as an open relay to hosts its
owner never intended, and any command or path injection through a peer-supplied
value.

The transport, its handshake and its access checks live in
[`p2p-common`](https://github.com/node-in-net/p2p-common) and are in scope of
that repository.

## What is out of scope

- Vulnerabilities in third-party crates — report those upstream, though we are
  glad to hear which of our dependencies is affected.
- What an authorised peer can legitimately do with a capability the node's owner
  deliberately enabled. A shared terminal grants shell access by design.
- Findings that require an attacker to already control the machine running the
  node.

## Supported versions

This repository is consumed as a submodule and has no release train yet.
Security fixes land on `main`; consuming projects update their submodule
pointer. Once versioned releases exist, this section will name the supported
ones.
