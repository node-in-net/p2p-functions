#![cfg(target_os = "windows")]

use crate::desktop::{CapturedFrame, DesktopStreamStatus};
use std::sync::Arc;
use windows_capture::capture::GraphicsCaptureApiHandler;

pub struct WindowsCaptureFlags<F, S> {
    pub stop_flag: Arc<std::sync::atomic::AtomicBool>,
    pub frame_callback: Arc<F>,
    pub status_callback: Arc<S>,
}

struct WindowsCaptureHandler<F, S> {
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
    is_first: bool,
}

impl<F, S> GraphicsCaptureApiHandler for WindowsCaptureHandler<F, S>
where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    type Flags = WindowsCaptureFlags<F, S>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: windows_capture::capture::Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            stop_flag: ctx.flags.stop_flag,
            frame_callback: ctx.flags.frame_callback,
            status_callback: ctx.flags.status_callback,
            is_first: true,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame,
        capture_control: windows_capture::graphics_capture_api::InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }

        let width = frame.width() as usize;
        let height = frame.height() as usize;

        let mut frame_buf = frame.buffer()?;
        let raw_pixels = frame_buf.as_raw_buffer();

        // Graphics Capture hands back RGBA; CapturedFrame is BGRA.
        let mut bgra_data = vec![0u8; width * height * 4];
        for i in (0..raw_pixels.len()).step_by(4) {
            if i + 3 < raw_pixels.len() {
                bgra_data[i] = raw_pixels[i + 2]; // Blue
                bgra_data[i + 1] = raw_pixels[i + 1]; // Green
                bgra_data[i + 2] = raw_pixels[i]; // Red
                bgra_data[i + 3] = raw_pixels[i + 3]; // Alpha
            }
        }

        if self.is_first {
            (self.status_callback)(DesktopStreamStatus::Active { width, height });
            self.is_first = false;
        }

        (self.frame_callback)(CapturedFrame {
            data: bgra_data,
            width,
            height,
        });

        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        (self.status_callback)(DesktopStreamStatus::Stopped);
        Ok(())
    }
}

pub fn start_windows_capture<F, S>(
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
) where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    let stop_flag_clone = stop_flag.clone();
    let frame_cb_clone = frame_callback.clone();
    let status_cb_clone = status_callback.clone();

    tokio::spawn(async move {
        (status_callback)(DesktopStreamStatus::Starting(
            "Initializing Windows Graphics Capture...".to_string(),
        ));

        let primary_monitor = match windows_capture::monitor::Monitor::primary() {
            Ok(m) => m,
            Err(e) => {
                (status_callback)(DesktopStreamStatus::Error(format!(
                    "No primary monitor found: {:?}",
                    e
                )));
                return;
            }
        };

        let settings = windows_capture::settings::Settings::new(
            primary_monitor,
            windows_capture::settings::CursorCaptureSettings::Default,
            windows_capture::settings::DrawBorderSettings::Default,
            windows_capture::settings::SecondaryWindowSettings::Default,
            windows_capture::settings::MinimumUpdateIntervalSettings::Default,
            windows_capture::settings::DirtyRegionSettings::Default,
            windows_capture::settings::ColorFormat::Rgba8,
            WindowsCaptureFlags {
                stop_flag: stop_flag_clone,
                frame_callback: frame_cb_clone,
                status_callback: status_cb_clone,
            },
        );

        if let Err(e) = WindowsCaptureHandler::start(settings) {
            (status_callback)(DesktopStreamStatus::Error(format!(
                "Windows capture session error: {:?}",
                e
            )));
        }
    });
}
