#![cfg(target_os = "macos")]

use crate::desktop::{CapturedFrame, DesktopStreamStatus};
use std::sync::Arc;
use std::time::Duration;

use screencapturekit::cm::CMTime;
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

struct MacosFrameHandler<F, S> {
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
    is_first: Arc<std::sync::atomic::AtomicBool>,
}

impl<F, S> SCStreamOutputTrait for MacosFrameHandler<F, S>
where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        if matches!(output_type, SCStreamOutputType::Screen) {
            if let Some(pixel_buffer) = sample.image_buffer() {
                match pixel_buffer.lock(CVPixelBufferLockFlags::READ_ONLY) {
                    Ok(guard) => {
                        let width = guard.width() as usize;
                        let height = guard.height() as usize;
                        let pixels = guard.as_slice();

                        let bgra_data = pixels.to_vec();

                        if self
                            .is_first
                            .swap(false, std::sync::atomic::Ordering::SeqCst)
                        {
                            (self.status_callback)(DesktopStreamStatus::Active { width, height });
                        }

                        (self.frame_callback)(CapturedFrame {
                            data: bgra_data,
                            width,
                            height,
                        });
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

pub fn start_macos_capture<F, S>(
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
        (status_cb_clone)(DesktopStreamStatus::Starting(
            "Initializing macOS ScreenCaptureKit...".to_string(),
        ));

        crate::mouse::check_and_request_accessibility_permissions();

        let content = match SCShareableContent::get() {
            Ok(c) => c,
            Err(e) => {
                (status_cb_clone)(DesktopStreamStatus::Error(format!(
                    "Failed to retrieve SCShareableContent: {:?}",
                    e
                )));
                return;
            }
        };

        let display = match content.displays().into_iter().next() {
            Some(d) => d,
            None => {
                (status_cb_clone)(DesktopStreamStatus::Error(
                    "No macOS displays found".to_string(),
                ));
                return;
            }
        };

        let filter = SCContentFilter::create().with_display(&display).build();

        let width = display.width();
        let height = display.height();

        let frame_interval = CMTime::new(1, 15); // ~15 FPS limit
        let config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_minimum_frame_interval(&frame_interval);

        let mut stream = SCStream::new(&filter, &config);

        let handler = MacosFrameHandler {
            frame_callback: frame_cb_clone,
            status_callback: status_cb_clone.clone(),
            is_first: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        if stream
            .add_output_handler(handler, SCStreamOutputType::Screen)
            .is_none()
        {
            (status_cb_clone)(DesktopStreamStatus::Error(
                "Failed to attach SCStream output handler".to_string(),
            ));
            return;
        }

        if let Err(e) = stream.start_capture() {
            (status_cb_clone)(DesktopStreamStatus::Error(format!(
                "Failed to initiate macOS stream capture: {:?}",
                e
            )));
            return;
        }

        while !stop_flag_clone.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let _ = stream.stop_capture();
        (status_cb_clone)(DesktopStreamStatus::Stopped);
    });
}
