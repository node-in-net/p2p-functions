//! The platform capture and input backends, behind the transport's
//! [`DesktopProvider`] trait.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use client_core::desktop::{
    CapturedFrame, DesktopProvider, DesktopStreamStatus, FrameCallback, StatusCallback,
};
use nodeinnet_p2p::DesktopInputEvent;

/// Serves this machine's screen: PipeWire on Linux, Windows Graphics Capture,
/// ScreenCaptureKit on macOS.
#[derive(Default)]
pub struct SystemDesktop;

impl DesktopProvider for SystemDesktop {
    fn start_capture(
        &self,
        stop_flag: Arc<AtomicBool>,
        force_select: bool,
        on_frame: FrameCallback,
        on_status: StatusCallback,
    ) {
        node_functions::desktop::start_desktop_stream(
            stop_flag,
            force_select,
            move |f: node_functions::desktop::CapturedFrame| {
                on_frame(CapturedFrame {
                    data: f.data,
                    width: f.width,
                    height: f.height,
                })
            },
            move |s: node_functions::desktop::DesktopStreamStatus| {
                on_status(match s {
                    node_functions::desktop::DesktopStreamStatus::Starting(m) => {
                        DesktopStreamStatus::Starting(m)
                    }
                    node_functions::desktop::DesktopStreamStatus::Active { width, height } => {
                        DesktopStreamStatus::Active { width, height }
                    }
                    node_functions::desktop::DesktopStreamStatus::Error(m) => {
                        DesktopStreamStatus::Error(m)
                    }
                    node_functions::desktop::DesktopStreamStatus::Stopped => {
                        DesktopStreamStatus::Stopped
                    }
                })
            },
        );
    }

    fn primary_screen_size(&self) -> Option<(usize, usize)> {
        node_functions::desktop::get_primary_screen_size()
    }

    fn apply_input(&self, event: &DesktopInputEvent) {
        node_functions::mouse::simulate_mouse_input(event);
    }
}
