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

use std::time::Duration;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::luminance::{weighted_luminance, Frame, LatestLuminance};

pub struct Flags {
    pub center_weight: f32,
    pub latest: LatestLuminance,
}

struct Handler {
    center_weight: f32,
    latest: LatestLuminance,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            center_weight: ctx.flags.center_weight,
            latest: ctx.flags.latest,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
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
    // Let the OS pace delivery instead of capturing at display rate and throwing
    // frames away: the pupil's fastest move is ~0.3s, so ~10/s is plenty and the
    // cost stays near zero.
    let interval = Duration::from_secs_f32(1.0 / fps.max(1) as f32);

    let settings = Settings::new(
        window,
        // The cursor is not part of the scene's lighting.
        CursorCaptureSettings::WithoutCursor,
        // No yellow capture border around VRChat.
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(interval),
        DirtyRegionSettings::Default,
        // BGRA8 is what `luminance` expects.
        ColorFormat::Bgra8,
        Flags {
            center_weight,
            latest,
        },
    );

    Handler::start(settings).map_err(|e| e.to_string())
}
