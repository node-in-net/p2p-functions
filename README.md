# p2p-functions

What a node does when a peer asks something of it over the
[node.in.net](https://node.in.net) peer-to-peer protocol: read a directory, send
a file, open a terminal, relay a TCP connection, capture a screen. An
application registers the parts it wants to serve.

The transport lives in
[`p2p-common`](https://github.com/node-in-net/p2p-common). It moves messages,
runs the handshake and decides whether a peer may touch a shared resource — but
it implements none of the work itself. It has no filesystem, no terminal, no
screen. This repository supplies those, and an application installs the ones it
wants at startup:

```rust
use p2p_handlers::Capabilities;

p2p_handlers::install(Capabilities::FILESYSTEM | Capabilities::NETWORK);
```

That call is the whole integration on this side; the application still declares
which resources it shares. Until it runs, a node connects and talks but serves
nothing.

## Crates

| Crate | What it is |
| --- | --- |
| `node-functions` | The work itself, one module per capability: local filesystem, PTY terminal, Windows registry, desktop capture and input injection, SOCKS and HTTP proxying, folder sync, system info. Never sees a `P2pMessage`. |
| `p2p-handlers` | The seam: maps each `P2pMessage` to the function that serves it, and registers itself into the transport. This is what an application calls. |
| `web-davserver` | WebDAV bridge — serves a remote peer's filesystem resource as a local WebDAV mount, so any WebDAV-capable file manager can browse a peer. |

## Choosing what to serve

```rust
// A file manager that shares files and nothing else.
p2p_handlers::install(Capabilities::FILESYSTEM);

// A phone sharing its files and its internet connection.
p2p_handlers::install(Capabilities::FILESYSTEM | Capabilities::NETWORK);

// A desktop serving everything this build compiled in.
p2p_handlers::install(Capabilities::ALL);
```

Registering nothing is a valid choice. Such a node still connects to peers and
uses everything *they* share — browsing a peer's files, viewing a peer's screen.
It simply offers nothing of its own. Consuming and serving are independent.

## Two gates

A capability is served only if it passes both:

- **Cargo features** decide what is *compiled in*. Every capability feature is on
  by default; a build that takes `p2p-handlers` with `default-features = false`
  and leaves `feature-rdesk` out drops the H.264 codec and the per-OS capture
  backends — a file manager does not compile a video encoder.
- **`Capabilities`** decides what this *process* serves. One binary can serve
  different sets depending on how the user configured it.

A capability that is off has no handler at all: nothing matches, and the message
is dropped. Separately, the transport refuses any message aimed at a resource
this node never shared, before it reaches this crate.

## Elevated privileges

Screen capture on Linux needs PipeWire, GStreamer and a working
`xdg-desktop-portal`, which a desktop may be missing. `node-functions` can
install them: `run_gstreamer_installer`, `run_video_codecs_installer` and
`run_portal_installer` shell out to `pkexec` — polkit's prompt — and run
`apt-get`, `dnf` or `pacman` as root, picked by the distribution.

Nothing calls them on its own. They are explicit entry points an application
offers the user, they never run as a side effect of capture, and they never run
in response to anything a peer sends. There is no equivalent on Windows or
macOS: those platforms ship their capture APIs with the OS, so nothing here
elevates on them.

## Dependencies

This repository depends on `p2p-common` (for the protocol types and the traits
it registers into) and on
[`common`](https://github.com/node-in-net/common). The dependency runs one way —
the transport never depends on these implementations.

## Building

There is no workspace manifest at the root: the consuming project defines the
workspace. Build from a project that includes this repository as a submodule;
from there, name the packages directly:

```sh
cargo check -p node-functions -p p2p-handlers -p web-davserver
```

`node-functions` compiles per-OS capture backends behind the `screen-capture`
feature (on by default), which needs the platform SDKs — PipeWire and GStreamer
on Linux, Windows Graphics Capture on Windows, ScreenCaptureKit on macOS.
`synthetic-capture` replaces them with animated frames so the remote-desktop
path can run headlessly in tests; applications never enable it.

## Contributing

Every commit needs a `Signed-off-by` line certifying the
[Developer Certificate of Origin](DCO); a CI check enforces it. See
[CONTRIBUTING.md](CONTRIBUTING.md).

No build workflow runs here: every crate resolves `p2p-common` and `common`
through a consuming workspace, so there is nothing to build from this repository
alone. The projects that embed it build and test these crates.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
