# Contributing to p2p-functions

Thanks for taking the time to contribute.

This repository implements what a node does for its peers over the
[node.in.net](https://node.in.net) protocol.

## Developer Certificate of Origin (sign-off required)

This project does **not** use a CLA. Instead, every commit must carry a
`Signed-off-by` line, certifying the [Developer Certificate of Origin](DCO)
(the full text is in the `DCO` file at the repository root).

Git adds the line for you:

```sh
git commit -s -m "your message"
```

It looks like this, and the e-mail must match the commit author's:

```
Signed-off-by: Jane Doe <jane@example.com>
```

To never forget it, install a hook — once per clone. Note that
`git config format.signoff` does **not** do this; it only affects
`git format-patch`:

```sh
printf '%s\n' '#!/bin/sh' 'git interpret-trailers --in-place --if-exists doNothing --trailer "Signed-off-by: $(git config user.name) <$(git config user.email)>" "$1"' > .git/hooks/prepare-commit-msg
chmod +x .git/hooks/prepare-commit-msg
```

It reads `user.name` and `user.email` from git's config, runs for `git commit`
from any editor or GUI, and does not add a second line when you already
passed `-s`.

Missing a sign-off on an existing commit? `git commit --amend -s` fixes the
last one; `git rebase --signoff <base>` fixes a whole branch. A CI check
enforces this on every pull request.

## Licensing of contributions

Unless you state otherwise, any contribution you submit is licensed under the
same terms as the project — **MIT OR Apache-2.0**, at the user's option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

The name "node.in.net" and the project logo are not covered by the code
license.

## Getting the source

```sh
git clone https://github.com/node-in-net/p2p-functions.git
```

There is no workspace manifest here: the consuming project defines the
workspace, and it resolves `p2p-common` and `common` for these crates. Work
inside a project that includes this repository as a submodule.

## Building

Rust (stable) is required, plus the platform capture SDKs when the
`screen-capture` feature is on (it is by default) — PipeWire and GStreamer
development packages on Linux, Windows Graphics Capture on Windows,
ScreenCaptureKit on macOS.

```sh
cargo check -p node-functions -p p2p-handlers -p web-davserver
```

Building without the capture backends:

```sh
cargo check -p node-functions --no-default-features
```

## Where a change belongs

- **`node-functions`** does the work against the operating system and knows
  nothing about the protocol. A function here takes plain arguments and returns
  plain results.
- **`p2p-handlers`** translates between the protocol and those functions.
  `node-functions` never sees a `P2pMessage`; keep it that way.

Adding a capability means both: the implementation in `node-functions`, its
message arm and a `Capabilities` bit in `p2p-handlers`. Put it behind a Cargo
feature so nodes that do not serve it do not compile it.

Nothing here may depend on `p2p-common` in reverse: the transport must never
need an implementation.

## Security-sensitive areas

Two places decide what a peer can reach, and changes to them deserve extra care:

- **Path sandboxing** in `node-functions/src/fs_local.rs` — a peer must not
  escape the root of a shared folder.
- **Proxy egress** in `node-functions/src/socks5.rs` and `web_proxy.rs` — this
  node opens connections on a peer's behalf, so it can be used as a relay.

## Before you open a pull request

- Keep changes focused and prefer reusing existing abstractions over adding new
  ones.
- Comments in English, and only where the code cannot speak for itself.
- Run:

```sh
cargo fmt --check -p node-functions -p p2p-handlers -p web-davserver
cargo clippy -p node-functions -p p2p-handlers -p web-davserver --all-targets
cargo test -p node-functions -p p2p-handlers -p web-davserver
```

Formatting and tests must be clean. Clippy is not yet: these crates still carry
warnings inherited from before they were split out. Do not add new ones — once
the backlog is cleared, `-D warnings` becomes part of CI.

## Reporting bugs

A good report includes the platform, the crate and version, and — most valuable
— concrete steps to reproduce. Capture and terminal problems are usually
platform-specific, so name the desktop session (X11 or Wayland, portal in use)
and the OS version.
