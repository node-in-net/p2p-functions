# p2p-handlers

The crate an application calls, part of [`p2p-functions`](../README.md) in the
[node.in.net](https://node.in.net) stack.

It is the seam between the protocol and the machine. On one side sits
[`p2p-common`](https://github.com/node-in-net/p2p-common), which moves messages
and guards access but implements nothing; on the other sits `node-functions`,
which works against the operating system but never sees a `P2pMessage`. This
crate maps each message to the function that serves it, and registers itself
into the transport.

```rust
use p2p_handlers::Capabilities;

p2p_handlers::install(Capabilities::FILESYSTEM | Capabilities::NETWORK);
```

One call, before connecting. After it, a peer asking for a directory listing
gets one; a peer asking for anything not installed gets nothing.

## Capabilities

| Bit | Serves | Cargo feature |
| --- | --- | --- |
| `FILESYSTEM` | Browsing and transferring files in the shared folders | `feature-fm` |
| `TERMINAL` | A remote PTY | `feature-terminal` |
| `NETWORK` | SOCKS tunnelling and HTTP proxying on the peer's behalf | `feature-net` |
| `REGISTRY` | The Windows registry; serves nothing elsewhere | `feature-registry` |
| `SYSTEM_INFO` | Hardware and OS telemetry | `feature-sysinfo` |
| `SYNC_FOLDER` | Folder synchronisation | `feature-sync` |
| `REMOTE_DESKTOP` | Sharing this screen and accepting remote input | `feature-rdesk` |

`Capabilities::ALL` is every bit; `Capabilities::NONE` is a node that consumes
what its peers share and offers nothing back.

Remote desktop is the one that does not work through message arms: `install`
registers a `DesktopProvider` for it instead, because the transport drives
capture itself once a peer is approved.

Why both a Cargo feature and a runtime bit, and why installing nothing is still
a valid choice, are covered in the [repository README](../README.md).

## License

[Apache License 2.0](../LICENSE-APACHE) or [MIT](../LICENSE-MIT), at your option.
