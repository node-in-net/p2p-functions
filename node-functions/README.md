# node-functions

The local half of [`p2p-functions`](../README.md), in the
[node.in.net](https://node.in.net) stack.

Each module does one kind of work against the operating system: the local
filesystem, a PTY terminal, the Windows registry, desktop capture and input
injection, SOCKS and HTTP proxying, folder sync, system info.

Nothing here speaks the protocol: no module ever sees a `P2pMessage`, and none
of them knows which peer asked. A function takes plain arguments and returns
plain results; `p2p-handlers` is what connects them to incoming messages.

Per-OS code is feature- and target-gated. `screen-capture` pulls in the platform
capture backends — PipeWire and GStreamer on Linux, Windows Graphics Capture,
ScreenCaptureKit on macOS — and `synthetic-capture` replaces them with animated
frames so the media path can be tested without a display.

## License

[Apache License 2.0](../LICENSE-APACHE) or [MIT](../LICENSE-MIT), at your option.
