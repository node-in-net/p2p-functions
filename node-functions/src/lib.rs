#[cfg(feature = "sysinfo")]
pub mod system_info;
#[cfg(feature = "terminal")]
pub mod terminal;

#[cfg(feature = "fs")]
pub mod fs_local;
pub mod names;
#[cfg(feature = "registry")]
pub mod registry;
#[cfg(feature = "net")]
pub mod web_proxy;

#[cfg(feature = "desktop")]
pub mod desktop;
#[cfg(feature = "desktop")]
pub mod mouse;
#[cfg(feature = "net")]
pub mod net_guard;
#[cfg(feature = "net")]
pub mod socks5;
#[cfg(feature = "sync")]
pub mod sync_folder;
