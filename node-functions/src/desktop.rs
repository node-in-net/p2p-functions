use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DesktopStreamStatus {
    Starting(String),
    Active { width: usize, height: usize },
    Error(String),
    Stopped,
}

#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub data: Vec<u8>, // Raw BGRA 32-bit pixel buffer
    pub width: usize,
    pub height: usize,
}

#[cfg(all(target_os = "linux", feature = "screen-capture"))]
#[path = "linux_desktop.rs"]
pub mod linux_desktop;

#[cfg(all(target_os = "windows", feature = "screen-capture"))]
#[path = "windows_desktop.rs"]
pub mod windows_desktop;

#[cfg(all(target_os = "macos", feature = "screen-capture"))]
#[path = "macos_desktop.rs"]
pub mod macos_desktop;

#[cfg(feature = "synthetic-capture")]
#[path = "synthetic_desktop.rs"]
pub mod synthetic_desktop;

pub fn start_desktop_stream<F, S>(
    stop_flag: Arc<AtomicBool>,
    force_select: bool,
    restore_token: Option<String>,
    on_restore_token: Arc<dyn Fn(String) + Send + Sync>,
    frame_callback: F,
    status_callback: S,
) where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    let stop_flag_clone = stop_flag.clone();
    let frame_cb = Arc::new(frame_callback);
    let status_cb = Arc::new(status_callback);
    let _ = (force_select, &restore_token, &on_restore_token);

    #[cfg(feature = "synthetic-capture")]
    {
        synthetic_desktop::start_synthetic_capture(stop_flag_clone, frame_cb, status_cb);
    }

    #[cfg(all(
        target_os = "linux",
        feature = "screen-capture",
        not(feature = "synthetic-capture")
    ))]
    {
        let frame_cb_inner = frame_cb.clone();
        let status_cb_inner = status_cb.clone();
        linux_desktop::start_linux_capture(
            stop_flag_clone,
            force_select,
            restore_token,
            on_restore_token,
            frame_cb_inner,
            status_cb_inner,
        );
    }

    #[cfg(all(
        target_os = "windows",
        feature = "screen-capture",
        not(feature = "synthetic-capture")
    ))]
    {
        let frame_cb_inner = frame_cb.clone();
        let status_cb_inner = status_cb.clone();
        windows_desktop::start_windows_capture(stop_flag_clone, frame_cb_inner, status_cb_inner);
    }

    #[cfg(all(
        target_os = "macos",
        feature = "screen-capture",
        not(feature = "synthetic-capture")
    ))]
    {
        let frame_cb_inner = frame_cb.clone();
        let status_cb_inner = status_cb.clone();
        macos_desktop::start_macos_capture(stop_flag_clone, frame_cb_inner, status_cb_inner);
    }

    #[cfg(all(
        not(all(
            any(target_os = "linux", target_os = "windows", target_os = "macos"),
            feature = "screen-capture"
        )),
        not(feature = "synthetic-capture")
    ))]
    {
        let _ = (stop_flag_clone, frame_cb);
        status_cb(DesktopStreamStatus::Error(
            "Screen capture is not available on this platform build".to_string(),
        ));
    }
}

pub fn is_gstreamer_pipewire_available() -> bool {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::is_gstreamer_pipewire_available()
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        false
    }
}

pub fn update_gstreamer_registry() {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::update_gstreamer_registry();
    }
}

pub async fn run_gstreamer_installer() -> Result<(), String> {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::run_gstreamer_installer().await
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        Err(
            "GStreamer installation is only supported on Linux with screen capture enabled."
                .to_string(),
        )
    }
}

pub fn is_video_decoding_available() -> bool {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::is_video_decoding_available()
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        false
    }
}

pub async fn run_video_codecs_installer() -> Result<(), String> {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::run_video_codecs_installer().await
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        Err("Video codecs installation is only supported on Linux.".to_string())
    }
}

pub fn get_primary_screen_size() -> Option<(usize, usize)> {
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    {
        if let Ok(screens) = screenshots::Screen::all() {
            if let Some(primary) = screens.first() {
                return Some((
                    primary.display_info.width as usize,
                    primary.display_info.height as usize,
                ));
            }
        }
    }
    None
}

pub fn is_xdg_desktop_portal_installed() -> bool {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::is_xdg_desktop_portal_installed()
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        true // Windows/macOS don't need a portal, so they always satisfy this check
    }
}

pub async fn run_portal_installer() -> Result<(), String> {
    #[cfg(all(target_os = "linux", feature = "screen-capture"))]
    {
        linux_desktop::run_portal_installer().await
    }
    #[cfg(not(all(target_os = "linux", feature = "screen-capture")))]
    {
        Err(
            "Portal installation is only supported on Linux with screen capture enabled."
                .to_string(),
        )
    }
}
