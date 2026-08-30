# web-davserver

A WebDAV bridge for the [node.in.net](https://node.in.net) stack, part of
[`p2p-functions`](../README.md).

Serves a remote peer's filesystem resource as a local WebDAV mount, translating
each request into protocol messages and the replies back into WebDAV responses.
Lets any WebDAV-capable file manager browse a peer without knowing about p2p.

## License

[Apache License 2.0](../LICENSE-APACHE) or [MIT](../LICENSE-MIT), at your option.
