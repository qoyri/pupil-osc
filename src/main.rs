// SPDX-License-Identifier: MIT
//
// Qoyri pupil-osc — drives an avatar's pupil from how bright the VRChat view is.
//
// A capture thread keeps the newest luminance in a shared slot; the main loop
// ticks at the configured rate, eases the pupil toward what that luminance calls
// for, and sends the -1..1 float to VRChat. Tuning lives in pupil-osc.toml next
// to the executable.
//
// Console app on purpose: the live readout is how you tune the brightness range
// against your own avatar.

// A console is the point here, so no windows_subsystem attribute.

#[cfg(target_os = "windows")]
mod app {
    use std::io::Write;
    use std::thread;
    use std::time::{Duration, Instant};

    use log::{error, info, warn};

    use pupil_osc::capture;
    use pupil_osc::config::Config;
    use pupil_osc::luminance::LatestLuminance;
    use pupil_osc::osc::PupilSender;
    use pupil_osc::pupil::Pupil;

    /// How long to wait before looking for the VRChat window again.
    const RETRY: Duration = Duration::from_secs(3);

    pub fn run() {
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("info,pupil_osc=info"),
        )
        .init();

        let config = Config::load();
        info!(
            "sending {} to 127.0.0.1:{} — measuring \"{}\" at {} fps",
            config.address(),
            config.osc_port,
            config.window_title,
            config.capture_fps
        );

        let latest = LatestLuminance::new();

        // Capture runs on its own thread and re-attaches on its own: VRChat may
        // not be running yet, and it may restart while we stay up.
        {
            let latest = latest.clone();
            let title = config.window_title.clone();
            let fps = config.capture_fps;
            let center_weight = config.center_weight;
            thread::spawn(move || capture_loop(&title, fps, center_weight, latest));
        }

        let mut sender = match PupilSender::new(config.osc_port, config.address()) {
            Ok(s) => s,
            Err(e) => {
                error!("could not open the OSC socket: {e}");
                return;
            }
        };

        let params = config.params();
        let mut pupil = Pupil::new();
        let tick = Duration::from_secs_f32(1.0 / config.capture_fps.max(1) as f32);
        let mut previous = Instant::now();

        loop {
            thread::sleep(tick);

            let now = Instant::now();
            let dt = now.duration_since(previous).as_secs_f32();
            previous = now;

            let luminance = latest.get();
            let value = pupil.step(luminance, dt, &params);

            match sender.send(value, now) {
                Ok(sent) => readout(luminance, value, sent),
                Err(e) => warn!("OSC send failed: {e}"),
            }
        }
    }

    /// Keeps a capture attached to the VRChat window for as long as it exists,
    /// and waits quietly when it does not.
    fn capture_loop(title: &str, fps: u32, center_weight: f32, latest: LatestLuminance) {
        let mut announced_missing = false;
        loop {
            match capture::find_window(title) {
                Some(window) => {
                    announced_missing = false;
                    info!("capturing \"{title}\"");
                    if let Err(e) = capture::run(window, fps, center_weight, latest.clone()) {
                        warn!("capture stopped: {e}");
                    } else {
                        info!("capture ended (window closed)");
                    }
                    // Whatever ended it, the scene is no longer visible: let the
                    // pupil settle open rather than freeze on the last reading.
                    latest.set(0.0);
                }
                None => {
                    if !announced_missing {
                        info!("waiting for a window matching \"{title}\"");
                        announced_missing = true;
                    }
                }
            }
            thread::sleep(RETRY);
        }
    }

    /// One self-overwriting status line — this is the tuning aid.
    fn readout(luminance: f32, value: f32, sent: bool) {
        let stops = luminance.max(1e-6).log2();
        print!(
            "\rluminance {luminance:>8.5}  ({stops:>6.1} stops)   pupil {value:>+6.3}  {}   ",
            if sent { "->" } else { "  " }
        );
        let _ = std::io::stdout().flush();
    }
}

#[cfg(target_os = "windows")]
fn main() {
    app::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    // The capture API is Windows-only; the measurement and response logic is
    // portable and covered by `cargo test` on any platform.
    println!("pupil-osc needs Windows: screen capture uses Windows.Graphics.Capture.");
}
