use gstreamer::prelude::Cast as GstCast;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::WindowIdentifier;

use crate::desktop::{CapturedFrame, DesktopStreamStatus};

pub fn start_linux_capture<F, S>(
    stop_flag: Arc<AtomicBool>,
    force_select: bool,
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
) where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    let stop_flag_clone = stop_flag.clone();
    let frame_cb = frame_callback.clone();
    let status_cb = status_callback.clone();

    tokio::spawn(async move {
        status_cb(DesktopStreamStatus::Starting(
            "Initializing Linux screencast portal...".to_string(),
        ));

        if let Err(e) = gstreamer::init() {
            status_cb(DesktopStreamStatus::Error(format!(
                "GStreamer initialization failed: {:?}",
                e
            )));
            status_cb(DesktopStreamStatus::Starting(
                "Falling back to standard screenshot loop...".to_string(),
            ));
            start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
            return;
        }

        let screencast_portal = match Screencast::new().await {
            Ok(p) => p,
            Err(e) => {
                status_cb(DesktopStreamStatus::Error(format!(
                    "Could not create Screencast portal: {:?}",
                    e
                )));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let session = match screencast_portal.create_session().await {
            Ok(s) => s,
            Err(e) => {
                status_cb(DesktopStreamStatus::Error(format!(
                    "Could not create screencast session: {:?}",
                    e
                )));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let config = client_config::AppConfig::new("nodeinnet");
        let restore_token: Option<String> = if force_select {
            None
        } else {
            config.get::<String>("rtc.screencast_restore_token")
        };

        if let Err(e) = screencast_portal
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Monitor | SourceType::Window,
                false,
                restore_token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await
        {
            status_cb(DesktopStreamStatus::Error(format!(
                "Could not configure screencast sources: {:?}",
                e
            )));
            status_cb(DesktopStreamStatus::Starting(
                "Falling back to standard screenshot loop...".to_string(),
            ));
            start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
            return;
        }

        let window_id = WindowIdentifier::default();
        let response = match screencast_portal.start(&session, &window_id).await {
            Ok(request) => match request.response() {
                Ok(streams) => {
                    if let Some(token) = streams.restore_token() {
                        config.set("rtc.screencast_restore_token", token);
                        config.save();
                    }
                    streams
                }
                Err(e) => {
                    status_cb(DesktopStreamStatus::Error(format!(
                        "Portal start denied or cancelled: {:?}",
                        e
                    )));
                    status_cb(DesktopStreamStatus::Starting(
                        "Falling back to standard screenshot loop...".to_string(),
                    ));
                    start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                    return;
                }
            },
            Err(e) => {
                status_cb(DesktopStreamStatus::Error(format!(
                    "Portal start request failed: {:?}",
                    e
                )));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let stream = match response.streams().first() {
            Some(s) => s,
            None => {
                status_cb(DesktopStreamStatus::Error(
                    "No streams returned by screencast portal".to_string(),
                ));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let fd = match screencast_portal.open_pipe_wire_remote(&session).await {
            Ok(fd) => fd,
            Err(e) => {
                status_cb(DesktopStreamStatus::Error(format!(
                    "Could not open PipeWire file descriptor: {:?}",
                    e
                )));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let raw_fd = fd.as_raw_fd();
        let node_id = stream.pipe_wire_node_id();

        let pipeline_str = format!(
            "pipewiresrc fd={} path={} autoconnect=true ! videoconvert ! video/x-raw,format=BGRA ! appsink name=sink",
            raw_fd, node_id
        );
        let parsed = match gstreamer::parse::launch(&pipeline_str) {
            Ok(p) => p,
            Err(e) => {
                status_cb(DesktopStreamStatus::Error(format!(
                    "Failed to parse GStreamer pipeline string: {:?}",
                    e
                )));
                status_cb(DesktopStreamStatus::Starting(
                    "Falling back to standard screenshot loop...".to_string(),
                ));
                start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
                return;
            }
        };

        let pipeline = GstCast::dynamic_cast::<gstreamer::Pipeline>(parsed).unwrap();
        let appsink = GstCast::dynamic_cast::<AppSink>(pipeline.by_name("sink").unwrap()).unwrap();

        let frame_cb_gst = frame_cb.clone();
        let status_cb_gst = status_cb.clone();
        let mut first_frame = true;

        appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gstreamer::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gstreamer::FlowError::Error)?;
                    let caps = sample.caps().ok_or(gstreamer::FlowError::Error)?;

                    let structure = caps.structure(0).ok_or(gstreamer::FlowError::Error)?;
                    let width = structure
                        .get::<i32>("width")
                        .map_err(|_| gstreamer::FlowError::Error)?
                        as usize;
                    let height = structure
                        .get::<i32>("height")
                        .map_err(|_| gstreamer::FlowError::Error)?
                        as usize;

                    let map = buffer
                        .map_readable()
                        .map_err(|_| gstreamer::FlowError::Error)?;
                    let raw_data = map.as_slice();

                    if raw_data.len() >= width * height * 4 {
                        if first_frame {
                            status_cb_gst(DesktopStreamStatus::Active { width, height });
                            first_frame = false;
                        }

                        frame_cb_gst(CapturedFrame {
                            data: raw_data[0..(width * height * 4)].to_vec(),
                            width,
                            height,
                        });
                    }

                    Ok(gstreamer::FlowSuccess::Ok)
                })
                .build(),
        );

        let tx_gst_err = status_cb.clone();
        let bus = pipeline.bus().unwrap();
        let _watch = bus
            .add_watch(move |_, msg| {
                use gstreamer::MessageView;
                if let MessageView::Error(err) = msg.view() {
                    tx_gst_err(DesktopStreamStatus::Error(format!(
                        "GStreamer pipeline error: {}",
                        err.error()
                    )));
                }
                gstreamer::glib::ControlFlow::Continue
            })
            .expect("Failed to add GStreamer bus watch");

        if let Err(e) = pipeline.set_state(gstreamer::State::Playing) {
            status_cb(DesktopStreamStatus::Error(format!(
                "Could not play the GStreamer capture stream: {:?}",
                e
            )));
            status_cb(DesktopStreamStatus::Starting(
                "Falling back to standard screenshot loop...".to_string(),
            ));
            start_screenshots_fallback_loop(stop_flag_clone, frame_cb, status_cb);
            return;
        }

        while !stop_flag_clone.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let _ = pipeline.set_state(gstreamer::State::Null);
        drop(fd);

        let _ = session.close().await;

        status_cb(DesktopStreamStatus::Stopped);
    });
}

pub fn start_screenshots_fallback_loop<F, S>(
    stop_flag: Arc<AtomicBool>,
    frame_callback: Arc<F>,
    status_callback: Arc<S>,
) where
    F: Fn(CapturedFrame) + Send + Sync + 'static,
    S: Fn(DesktopStreamStatus) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        status_callback(DesktopStreamStatus::Starting(
            "Initializing screenshots fallback loop...".to_string(),
        ));

        let screens = match screenshots::Screen::all() {
            Ok(s) => s,
            Err(e) => {
                status_callback(DesktopStreamStatus::Error(format!(
                    "Fallback capture error: Failed to query screens: {:?}",
                    e
                )));
                return;
            }
        };

        let primary_screen = match screens.first() {
            Some(s) => s,
            None => {
                status_callback(DesktopStreamStatus::Error(
                    "Fallback capture error: No screens found".to_string(),
                ));
                return;
            }
        };

        status_callback(DesktopStreamStatus::Active {
            width: primary_screen.display_info.width as usize,
            height: primary_screen.display_info.height as usize,
        });

        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            let start = std::time::Instant::now();

            if let Ok(image) = primary_screen.capture() {
                let width = image.width() as usize;
                let height = image.height() as usize;
                let raw_rgba = image.as_raw();

                let mut bgra_data = vec![0u8; width * height * 4];
                for i in (0..raw_rgba.len()).step_by(4) {
                    if i + 3 < raw_rgba.len() {
                        bgra_data[i] = raw_rgba[i + 2]; // Blue
                        bgra_data[i + 1] = raw_rgba[i + 1]; // Green
                        bgra_data[i + 2] = raw_rgba[i]; // Red
                        bgra_data[i + 3] = raw_rgba[i + 3]; // Alpha
                    }
                }

                frame_callback(CapturedFrame {
                    data: bgra_data,
                    width,
                    height,
                });
            }

            let elapsed = start.elapsed();
            let target_delay = Duration::from_millis(66); // ~15 FPS limit
            if elapsed < target_delay {
                tokio::time::sleep(target_delay - elapsed).await;
            } else {
                tokio::task::yield_now().await;
            }
        }

        status_callback(DesktopStreamStatus::Stopped);
    });
}

pub fn is_gstreamer_pipewire_available() -> bool {
    if gstreamer::init().is_err() {
        return false;
    }
    gstreamer::ElementFactory::find("pipewiresrc").is_some()
}

pub fn update_gstreamer_registry() {
    unsafe {
        gstreamer::ffi::gst_update_registry();
    }
}

pub fn detect_linux_distro() -> Result<String, String> {
    let os_release_path = std::path::Path::new("/etc/os-release");
    if !os_release_path.exists() {
        return Err("Cannot find /etc/os-release file to detect distribution.".to_string());
    }

    let content = std::fs::read_to_string(os_release_path)
        .map_err(|e| format!("Failed to read /etc/os-release: {}", e))?;

    let mut id = None;
    let mut id_like = None;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("ID=") {
            id = Some(
                line.trim_start_matches("ID=")
                    .trim_matches('"')
                    .to_lowercase(),
            );
        } else if line.starts_with("ID_LIKE=") {
            id_like = Some(
                line.trim_start_matches("ID_LIKE=")
                    .trim_matches('"')
                    .to_lowercase(),
            );
        }
    }

    if let Some(dist) = id {
        if dist.contains("ubuntu")
            || dist.contains("debian")
            || dist.contains("pop")
            || dist.contains("mint")
        {
            return Ok("ubuntu".to_string());
        }
        if dist.contains("fedora") || dist.contains("rhel") || dist.contains("centos") {
            return Ok("fedora".to_string());
        }
        if dist.contains("arch") || dist.contains("manjaro") {
            return Ok("arch".to_string());
        }
    }

    if let Some(like) = id_like {
        if like.contains("ubuntu") || like.contains("debian") {
            return Ok("ubuntu".to_string());
        }
        if like.contains("fedora") || like.contains("rhel") {
            return Ok("fedora".to_string());
        }
        if like.contains("arch") {
            return Ok("arch".to_string());
        }
    }

    Err("Unknown Linux distribution. Only Debian/Ubuntu, Fedora, and Arch Linux are supported for automatic installation.".to_string())
}

pub fn is_xdg_desktop_portal_installed() -> bool {
    if std::process::Command::new("which")
        .arg("xdg-desktop-portal")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    std::path::Path::new("/usr/libexec/xdg-desktop-portal").exists()
        || std::path::Path::new("/usr/lib/xdg-desktop-portal").exists()
}

pub async fn run_package_installer(
    arch_pkgs: &[&str],
    fedora_pkgs: &[&str],
    ubuntu_pkgs: &[&str],
) -> Result<(), String> {
    let distro = detect_linux_distro()?;

    let (cmd, args): (&str, Vec<String>) = match distro.as_str() {
        "ubuntu" | "debian" => {
            let mut sh_cmd = "apt-get update && apt-get install -y".to_string();
            for pkg in ubuntu_pkgs {
                sh_cmd = format!("{} {}", sh_cmd, pkg);
            }
            ("pkexec", vec!["sh".to_string(), "-c".to_string(), sh_cmd])
        }
        "fedora" => {
            let mut arg_list = vec!["dnf".to_string(), "install".to_string(), "-y".to_string()];
            for pkg in fedora_pkgs {
                arg_list.push(pkg.to_string());
            }
            ("pkexec", arg_list)
        }
        "arch" => {
            let mut arg_list = vec![
                "pacman".to_string(),
                "-S".to_string(),
                "--noconfirm".to_string(),
            ];
            for pkg in arch_pkgs {
                arg_list.push(pkg.to_string());
            }
            ("pkexec", arg_list)
        }
        _ => {
            return Err(format!(
                "Unsupported distribution: {}. Please install required packages manually.",
                distro
            ));
        }
    };

    let mut child = tokio::process::Command::new(cmd)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute pkexec: {}", e))?;

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Command waiting failed: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        use tokio::io::AsyncReadExt;
        let mut err_output = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err_output).await;
        }
        if err_output.trim().is_empty() {
            Err(
                "Authentication failed, cancelled, or installer returned non-zero exit code."
                    .to_string(),
            )
        } else {
            Err(err_output)
        }
    }
}

pub async fn run_gstreamer_installer() -> Result<(), String> {
    run_package_installer(
        &["gst-plugin-pipewire", "gst-plugins-good"],
        &["pipewire-gstreamer", "gstreamer1-plugins-good"],
        &["gstreamer1.0-pipewire", "gstreamer1.0-plugins-good"],
    )
    .await
}

pub async fn run_portal_installer() -> Result<(), String> {
    run_package_installer(
        &["xdg-desktop-portal", "xdg-desktop-portal-gnome"],
        &["xdg-desktop-portal", "xdg-desktop-portal-gnome"],
        &["xdg-desktop-portal", "xdg-desktop-portal-gnome"],
    )
    .await
}

pub fn is_video_decoding_available() -> bool {
    if gstreamer::init().is_err() {
        return false;
    }
    gstreamer::ElementFactory::find("avdec_h264").is_some()
        || gstreamer::ElementFactory::find("openh264dec").is_some()
}

pub async fn run_video_codecs_installer() -> Result<(), String> {
    run_package_installer(
        &[
            "gst-plugins-good",
            "gst-plugins-bad",
            "gst-plugins-ugly",
            "gst-libav",
        ],
        &[
            "gstreamer1-plugins-good",
            "gstreamer1-plugins-bad-free",
            "gstreamer1-plugins-ugly-free",
            "gstreamer1-libav",
        ],
        &[
            "gstreamer1.0-plugins-good",
            "gstreamer1.0-plugins-bad",
            "gstreamer1.0-plugins-ugly",
            "gstreamer1.0-libav",
        ],
    )
    .await
}
