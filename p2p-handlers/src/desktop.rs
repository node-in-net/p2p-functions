
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use client_core::desktop::{
    CapturedFrame, DesktopProvider, DesktopStreamStatus, FrameCallback, StatusCallback,
};
use nodeinnet_p2p::DesktopInputEvent;

#[derive(Default)]
pub struct SystemDesktop {
    restore_token: Option<String>,
    on_restore_token: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl SystemDesktop {
    pub fn new(
        restore_token: Option<String>,
        on_restore_token: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> Self {
        Self {
            restore_token,
            on_restore_token,
        }
    }
}

impl DesktopProvider for SystemDesktop {
    fn start_capture(
        &self,
        stop_flag: Arc<AtomicBool>,
        force_select: bool,
        on_frame: FrameCallback,
        on_status: StatusCallback,
    ) {
        let sink: Arc<dyn Fn(String) + Send + Sync> = match &self.on_restore_token {
            Some(sink) => sink.clone(),
            None => Arc::new(|_| {}),
        };
        node_functions::desktop::start_desktop_stream(
            stop_flag,
            force_select,
            self.restore_token.clone(),
            sink,
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
