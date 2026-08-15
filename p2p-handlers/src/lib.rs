//! The capabilities this node serves to its peers.
//!
//! An application picks which to serve and installs them once at startup:
//!
//! ```no_run
//! use p2p_handlers::Capabilities;
//! p2p_handlers::install(Capabilities::FILESYSTEM | Capabilities::NETWORK);
//! ```
//!
//! Both gates must pass: Cargo features decide what is compiled in, and
//! [`Capabilities`] what this process serves, so one binary can serve different
//! sets depending on how it was started.

use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use nodeinnet_p2p::P2pMessage;
use p2p_node::{MessageHandler, NodeContext};

#[cfg(feature = "feature-rdesk")]
pub mod desktop;
#[cfg(feature = "feature-fm")]
pub mod fs_local;
#[cfg(feature = "feature-net")]
pub mod net_config;
#[cfg(all(feature = "feature-registry", target_os = "windows"))]
pub mod registry;
#[cfg(feature = "feature-net")]
pub mod socks;
#[cfg(feature = "feature-sync")]
pub mod sync_folder;
#[cfg(feature = "feature-sysinfo")]
pub mod system_info;
#[cfg(feature = "feature-terminal")]
pub mod terminal;
#[cfg(feature = "feature-net")]
pub mod web_proxy;

/// Which capabilities a node serves. Combine them with `|`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities(u32);

impl Capabilities {
    /// Serve nothing — a client that only consumes remote resources.
    pub const NONE: Self = Self(0);
    /// Browse and transfer files in the shared folders.
    pub const FILESYSTEM: Self = Self(1 << 0);
    /// A remote PTY.
    pub const TERMINAL: Self = Self(1 << 1);
    /// SOCKS tunnelling and HTTP proxying on the peer's behalf.
    pub const NETWORK: Self = Self(1 << 2);
    /// The Windows registry. Serves nothing on other platforms.
    pub const REGISTRY: Self = Self(1 << 3);
    /// Hardware and OS telemetry.
    pub const SYSTEM_INFO: Self = Self(1 << 4);
    /// Folder synchronisation.
    pub const SYNC_FOLDER: Self = Self(1 << 5);
    /// Share this machine's screen and accept remote input.
    pub const REMOTE_DESKTOP: Self = Self(1 << 6);

    /// Every defined capability, regardless of which features are compiled in.
    pub const ALL: Self = Self(0b111_1111);

    /// True when every bit of `other` is set here.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw bits, for storing a choice in config or passing across FFI.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Rebuild from raw bits, ignoring any that are not defined.
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }
}

impl BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Capabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Routes an incoming message to the capability that serves it, if enabled.
pub struct NodeHandlers {
    enabled: Capabilities,
}

impl NodeHandlers {
    pub fn new(enabled: Capabilities) -> Self {
        Self { enabled }
    }
}

#[async_trait::async_trait]
impl MessageHandler for NodeHandlers {
    async fn handle(&self, msg: P2pMessage, ctx: NodeContext) {
        match msg {
            #[cfg(feature = "feature-fm")]
            P2pMessage::RequestEntries { .. }
            | P2pMessage::RequestMetadata { .. }
            | P2pMessage::CreateDirectoryRequest { .. }
            | P2pMessage::DeleteEntryRequest { .. }
            | P2pMessage::RenameEntryRequest { .. }
            | P2pMessage::SetPermissionsRequest { .. }
            | P2pMessage::FileUploadRequest { .. }
            | P2pMessage::FileDownloadRequest { .. }
            | P2pMessage::FileTransferResponse { .. }
            | P2pMessage::FileChunk { .. }
            | P2pMessage::FileTransferComplete { .. }
                if self.enabled.contains(Capabilities::FILESYSTEM) =>
            {
                fs_local::handle_local_fs_message(msg, ctx).await;
            }

            #[cfg(feature = "feature-sysinfo")]
            P2pMessage::RequestSystemInfo { .. }
                if self.enabled.contains(Capabilities::SYSTEM_INFO) =>
            {
                system_info::handle_system_info_message(msg, ctx).await;
            }

            #[cfg(feature = "feature-terminal")]
            P2pMessage::StartTerminal { .. }
            | P2pMessage::StopTerminal { .. }
            | P2pMessage::TerminalInput { .. }
            | P2pMessage::TerminalResize { .. }
                if self.enabled.contains(Capabilities::TERMINAL) =>
            {
                terminal::handle_terminal_message(msg, ctx).await;
            }

            #[cfg(feature = "feature-net")]
            P2pMessage::SocksConnectRequest { .. }
            | P2pMessage::SocksData { .. }
            | P2pMessage::SocksClose { .. }
                if self.enabled.contains(Capabilities::NETWORK) =>
            {
                socks::handle_socks_message(msg, ctx).await;
            }

            #[cfg(feature = "feature-net")]
            P2pMessage::HttpRequest { .. } if self.enabled.contains(Capabilities::NETWORK) => {
                web_proxy::handle_web_proxy_message(msg, ctx).await;
            }

            #[cfg(all(feature = "feature-registry", target_os = "windows"))]
            P2pMessage::RequestRegistryKeys { .. }
            | P2pMessage::CreateRegistryKeyRequest { .. }
            | P2pMessage::DeleteRegistryEntryRequest { .. }
            | P2pMessage::SetRegistryValueRequest { .. }
                if self.enabled.contains(Capabilities::REGISTRY) =>
            {
                registry::handle_registry_message(msg, ctx).await;
            }

            #[cfg(feature = "feature-sync")]
            P2pMessage::SyncStateRequest { .. } | P2pMessage::SyncStateResponse { .. }
                if self.enabled.contains(Capabilities::SYNC_FOLDER) =>
            {
                tokio::spawn(async move {
                    sync_folder::handle_sync_folder_message(msg, ctx).await;
                });
            }

            _ => {}
        }
    }
}

/// Serve `enabled` for the rest of this process. Call once, before connecting.
///
/// Screen sharing is installed separately from the message handlers because the
/// transport drives capture itself once a peer is approved.
pub fn install(enabled: Capabilities) {
    let _ = p2p_node::install_message_handler(Arc::new(NodeHandlers::new(enabled)));

    #[cfg(feature = "feature-rdesk")]
    if enabled.contains(Capabilities::REMOTE_DESKTOP) {
        let _ = client_core::desktop::install_desktop_provider(Arc::new(desktop::SystemDesktop));
    }
}

#[cfg(test)]
mod tests {
    use super::Capabilities;

    #[test]
    fn combines_and_tests_membership() {
        let caps = Capabilities::FILESYSTEM | Capabilities::NETWORK;
        assert!(caps.contains(Capabilities::FILESYSTEM));
        assert!(caps.contains(Capabilities::NETWORK));
        assert!(!caps.contains(Capabilities::TERMINAL));
    }

    #[test]
    fn none_contains_nothing_and_all_contains_everything() {
        assert!(!Capabilities::NONE.contains(Capabilities::FILESYSTEM));
        assert!(Capabilities::ALL.contains(Capabilities::REGISTRY));
        assert!(Capabilities::ALL.contains(Capabilities::SYNC_FOLDER));
    }

    #[test]
    fn round_trips_through_raw_bits() {
        let caps = Capabilities::TERMINAL | Capabilities::SYSTEM_INFO;
        assert_eq!(Capabilities::from_bits_truncate(caps.bits()), caps);
    }

    #[test]
    fn undefined_bits_are_dropped() {
        assert_eq!(
            Capabilities::from_bits_truncate(u32::MAX),
            Capabilities::ALL
        );
    }
}
