// SPDX-License-Identifier: MIT
//
// The only Windows-specific code in the program, and the only code no test here
// can reach — so it does as little as possible.
//
// It targets the VRChat *window* rather than the screen: Windows.Graphics.Capture
// keeps delivering a window's contents while it is occluded or you have alt-tabbed
// away, so the pupil keeps following the game instead of your browser. It also
// works the same whether VRChat is windowed, borderless or mirroring a headset.
//
// A frame is reduced to a single number right here in the callback, where the
// pixels already live. Nothing larger than an f32 crosses a thread boundary.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::{
    Error as WgcError, GraphicsCaptureApi, InternalCaptureControl,
};
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::luminance::{weighted_luminance, Frame, LatestLuminance};

/// Every non-default capture setting is gated on a Windows build that supports
/// it, and asking for an unsupported one aborts the whole capture rather than
/// degrading. So each is probed first and dropped to the default if missing —
/// none of them is worth losing the capture over.
///
/// Results are cached: capture is re-attached whenever VRChat restarts, and the
/// answer cannot change while the program runs.
fn probe(slot: &'static OnceLock<bool>, what: &str, f: fn() -> Result<bool, WgcError>) -> bool {
    *slot.get_or_init(|| match f() {
        Ok(true) => true,
        Ok(false) => {
            log::info!("this Windows build does not support {what}; using the default");
            false
        }
        Err(e) => {
            log::warn!("could not probe {what} ({e}); using the default");
            false
        }
    })
}

pub struct Flags {
    pub center_weight: f32,
    pub latest: LatestLuminance,
    /// Set when the OS would not throttle delivery for us, so the handler has to
    /// drop frames itself instead of measuring at full display rate.
    pub self_throttle: Option<Duration>,
}

struct Handler {
    center_weight: f32,
    latest: LatestLuminance,
    self_throttle: Option<Duration>,
    last_measured: Option<Instant>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            center_weight: ctx.flags.center_weight,
            latest: ctx.flags.latest,
            self_throttle: ctx.flags.self_throttle,
            last_measured: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // When the OS would not pace delivery, frames arrive at display rate.
        // Measuring every one of them would burn CPU for a signal the pupil
        // cannot use, so drop the extras here.
        if let Some(interval) = self.self_throttle {
            let now = Instant::now();
            if let Some(last) = self.last_measured {
                if now.duration_since(last) < interval {
                    return Ok(());
                }
            }
            self.last_measured = Some(now);
        }

        let mut buffer = frame.buffer()?;
        let width = buffer.width();
        let height = buffer.height();
        let row_pitch = buffer.row_pitch();
        let pixels = buffer.as_raw_buffer();

        let value = weighted_luminance(
            &Frame {
                pixels,
                width,
                height,
                row_pitch,
            },
            self.center_weight,
        );
        self.latest.set(value);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // VRChat exited or the window went away. Returning ends `start`, and the
        // caller goes back to looking for the window.
        Ok(())
    }
}

/// Finds the window whose title contains `title`, or None when it is not up yet.
pub fn find_window(title: &str) -> Option<Window> {
    Window::from_contains_name(title).ok()
}

/// Captures until the window closes or the capture fails, writing each frame's
/// luminance into `latest`. Blocks the calling thread.
pub fn run(
    window: Window,
    fps: u32,
    center_weight: f32,
    latest: LatestLuminance,
) -> Result<(), String> {
    let interval = Duration::from_secs_f32(1.0 / fps.max(1) as f32);

    // The cursor is not part of the scene's lighting. Available since Windows 10
    // 2004 (build 19041), so this one usually holds.
    static CURSOR: OnceLock<bool> = OnceLock::new();
    static BORDER: OnceLock<bool> = OnceLock::new();
    static INTERVAL: OnceLock<bool> = OnceLock::new();

    let cursor = if probe(
        &CURSOR,
        "hiding the cursor from the capture",
        GraphicsCaptureApi::is_cursor_settings_supported,
    ) {
        CursorCaptureSettings::WithoutCursor
    } else {
        CursorCaptureSettings::Default
    };

    // Suppresses the yellow capture border. Needs build 20348+, so Windows 10
    // 22H2 and earlier do not have it — and they do not draw the border either,
    // which is why falling back costs nothing there.
    let border = if probe(
        &BORDER,
        "removing the capture border",
        GraphicsCaptureApi::is_border_settings_supported,
    ) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };

    // Letting the OS pace delivery is the cheap way to run at capture_fps, but
    // MinUpdateInterval is one of the newest properties on the session. Without
    // it frames arrive at display rate and the handler throttles instead.
    let (update_interval, self_throttle) = if probe(
        &INTERVAL,
        "letting the OS pace frame delivery",
        GraphicsCaptureApi::is_minimum_update_interval_supported,
    ) {
        (MinimumUpdateIntervalSettings::Custom(interval), None)
    } else {
        (MinimumUpdateIntervalSettings::Default, Some(interval))
    };

    let settings = Settings::new(
        window,
        cursor,
        border,
        SecondaryWindowSettings::Default,
        update_interval,
        DirtyRegionSettings::Default,
        // BGRA8 is what `luminance` expects.
        ColorFormat::Bgra8,
        Flags {
            center_weight,
            latest,
            self_throttle,
        },
    );

    Handler::start(settings).map_err(|e| e.to_string())
}
