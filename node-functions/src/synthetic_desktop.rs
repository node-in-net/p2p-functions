use crate::desktop::{CapturedFrame, DesktopStreamStatus};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

fn draw_char(bgra: &mut [u8], width: usize, height: usize, x: usize, y: usize, ch: char) {
    let bitmap = match ch {
        '0' => [0x3c, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3c, 0x00],
        '1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x3c, 0x00],
        '2' => [0x3c, 0x66, 0x06, 0x1c, 0x30, 0x60, 0x7e, 0x00],
        '3' => [0x3c, 0x66, 0x06, 0x1c, 0x06, 0x66, 0x3c, 0x00],
        '4' => [0x66, 0x66, 0x66, 0x7e, 0x06, 0x06, 0x06, 0x00],
        '5' => [0x7e, 0x60, 0x7c, 0x06, 0x06, 0x66, 0x3c, 0x00],
        '6' => [0x3c, 0x60, 0x7c, 0x66, 0x66, 0x66, 0x3c, 0x00],
        '7' => [0x7e, 0x06, 0x0c, 0x18, 0x30, 0x30, 0x30, 0x00],
        '8' => [0x3c, 0x66, 0x66, 0x3c, 0x66, 0x66, 0x3c, 0x00],
        '9' => [0x3c, 0x66, 0x66, 0x3e, 0x06, 0x66, 0x3c, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x7e, 0x00, 0x00, 0x00, 0x00],
        ':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    };

    for row in 0..8 {
        let row_bits = bitmap[row];
        for col in 0..8 {
            if (row_bits & (0x80 >> col)) != 0 {
                for dy in 0..2 {
                    for dx in 0..2 {
                        let px = x + col * 2 + dx;
                        let py = y + row * 2 + dy;
                        if px < width && py < height {
                            let idx = (py * width + px) * 4;
                            if idx + 3 < bgra.len() {
                                bgra[idx] = 255; // Blue
                                bgra[idx + 1] = 255; // Green
                                bgra[idx + 2] = 255; // Red
                                bgra[idx + 3] = 255; // Alpha
                            }
                        }
                    }
                }
            }
        }
    }
}

fn draw_text(
    bgra: &mut [u8],
    width: usize,
    height: usize,
    start_x: usize,
    start_y: usize,
    text: &str,
) {
    let mut current_x = start_x;
    for ch in text.chars() {
        draw_char(bgra, width, height, current_x, start_y, ch);
        current_x += 18;
    }
}

pub fn generate_bgra_frame(width: usize, height: usize, frame_num: usize) -> Vec<u8> {
    let mut frame = vec![0u8; width * height * 4];

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - (width as f32 / 2.0);
            let dy = y as f32 - (height as f32 / 2.0);
            let dist = (dx * dx + dy * dy).sqrt();
            let val = ((dist * 0.1 - frame_num as f32 * 0.4).sin() * 127.0 + 128.0) as u8;

            let idx = (y * width + x) * 4;
            frame[idx] = val; // Blue
            frame[idx + 1] = val / 2; // Green
            frame[idx + 2] = 255 - val; // Red
            frame[idx + 3] = 255; // Alpha
        }
    }

    let now = chrono::Local::now();
    let time_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
    draw_text(&mut frame, width, height, 20, 20, &time_str);

    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_char_draws_nothing() {
        let mut buf = vec![0u8; 100 * 100 * 4];
        draw_char(&mut buf, 100, 100, 0, 0, 'X');
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn digit_zero_draws_some_pixels() {
        let mut buf = vec![0u8; 100 * 100 * 4];
        draw_char(&mut buf, 100, 100, 0, 0, '0');
        assert!(buf.iter().any(|&b| b == 255));
    }

    #[test]
    fn dash_draws_nothing_in_first_three_rows() {
        let mut buf = vec![0u8; 100 * 100 * 4];
        draw_char(&mut buf, 100, 100, 0, 0, '-');
        for y in 0..6usize {
            for x in 0..100usize {
                let idx = (y * 100 + x) * 4;
                assert_eq!(buf[idx], 0, "Expected no pixel at ({x}, {y})");
            }
        }
    }

    #[test]
    fn generate_frame_correct_size() {
        let frame = generate_bgra_frame(320, 240, 0);
        assert_eq!(frame.len(), 320 * 240 * 4);
    }

    #[test]
    fn generate_frame_alpha_channel_is_opaque() {
        let frame = generate_bgra_frame(10, 10, 0);
        for (i, &byte) in frame.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(byte, 255, "Alpha at pixel {} is not opaque", i / 4);
            }
        }
    }
}

pub fn start_synthetic_capture<F, S>(
    stop_flag: Arc<AtomicBool>,
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
) where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        status_callback(DesktopStreamStatus::Starting(
            "Initializing synthetic frame generator...".to_string(),
        ));

        let width = 800;
        let height = 600;
        let mut frame_num = 0;

        status_callback(DesktopStreamStatus::Active { width, height });

        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            let start = std::time::Instant::now();

            let data = generate_bgra_frame(width, height, frame_num);
            frame_num += 1;

            frame_callback(CapturedFrame {
                data,
                width,
                height,
            });

            let elapsed = start.elapsed();
            let target_delay = Duration::from_millis(66); // ~15 FPS
            if elapsed < target_delay {
                tokio::time::sleep(target_delay - elapsed).await;
            } else {
                tokio::task::yield_now().await;
            }
        }

        status_callback(DesktopStreamStatus::Stopped);
    });
}
