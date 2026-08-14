// SPDX-License-Identifier: MIT
//
// Tuning, read from `pupil-osc.toml` next to the executable. Everything worth
// adjusting lives here rather than in the code, because getting the pupil to
// look right on a given avatar is iteration, and a rebuild per attempt would
// make that miserable. Missing file or missing field falls back to a default
// that already works.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::pupil::Params;

/// Written next to the executable on first run so there is something to edit.
pub const FILE_NAME: &str = "pupil-osc.toml";

/// Shipped as-is when no config exists yet — the comments are the documentation,
/// which serde's serializer could not produce.
pub const TEMPLATE: &str = r#"# Qoyri pupil-osc — scene brightness -> avatar pupil size.

# The avatar parameter to drive. VRChat floats run -1..1:
# -1 = smallest pupil (bright scene), +1 = widest (dark scene).
parameter = "PupilSize"

# VRChat's OSC input port, and the window to measure (substring match on title).
osc_port = 9000
window_title = "VRChat"

# Frames measured per second. The fastest pupil movement is ~0.3s, so there is
# nothing to gain above ~15.
capture_fps = 10

# How much the middle of the view counts for. 0 = flat average of the whole
# window, 1 = almost only the centre. In between keeps the periphery heard.
center_weight = 0.6

# The brightness range the pupil spans, in stops (log2 of linear luminance).
# Raise bright_stops if the pupil sits pinned shut; lower dark_stops if it never
# fully opens in dark worlds.
bright_stops = -1.0
dark_stops = -7.0

# Inertia, seconds. Constriction is fast and dilation is slow in a real eye;
# keeping them apart is what makes it read as alive.
constrict_seconds = 0.3
dilate_seconds = 4.0
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub parameter: String,
    pub osc_port: u16,
    pub window_title: String,
    pub capture_fps: u32,
    pub center_weight: f32,
    pub bright_stops: f32,
    pub dark_stops: f32,
    pub constrict_seconds: f32,
    pub dilate_seconds: f32,
}

impl Default for Config {
    fn default() -> Self {
        let p = Params::default();
        Self {
            parameter: "PupilSize".to_string(),
            osc_port: 9000,
            window_title: "VRChat".to_string(),
            capture_fps: 10,
            center_weight: 0.6,
            bright_stops: p.bright_stops,
            dark_stops: p.dark_stops,
            constrict_seconds: p.constrict_seconds,
            dilate_seconds: p.dilate_seconds,
        }
    }
}

impl Config {
    /// Path of the config file: next to the executable, so the whole tool stays
    /// one folder you can move around.
    pub fn path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join(FILE_NAME))
    }

    /// Reads the config, writing the documented template first if none exists.
    /// Any failure degrades to defaults: a bad config file should not stop the
    /// pupil from working.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => match toml::from_str::<Self>(&raw) {
                Ok(config) => config.validated(),
                Err(e) => {
                    log::warn!("{FILE_NAME} is not valid, using defaults: {e}");
                    Self::default()
                }
            },
            Err(_) => {
                if let Err(e) = std::fs::write(&path, TEMPLATE) {
                    log::warn!("could not write {}: {e}", path.display());
                } else {
                    log::info!("wrote a default {} to tune", path.display());
                }
                Self::default()
            }
        }
    }

    /// Clamps hand-edited values into ranges the rest of the program can trust,
    /// instead of letting a typo produce a division by zero or a frozen pupil.
    pub fn validated(mut self) -> Self {
        self.center_weight = self.center_weight.clamp(0.0, 1.0);
        self.capture_fps = self.capture_fps.clamp(1, 60);
        self.constrict_seconds = self.constrict_seconds.max(0.0);
        self.dilate_seconds = self.dilate_seconds.max(0.0);
        if (self.dark_stops - self.bright_stops).abs() < 0.1 {
            log::warn!(
                "bright_stops and dark_stops are too close ({} vs {}), falling back",
                self.bright_stops,
                self.dark_stops
            );
            let p = Params::default();
            self.bright_stops = p.bright_stops;
            self.dark_stops = p.dark_stops;
        }
        self
    }

    pub fn params(&self) -> Params {
        Params {
            bright_stops: self.bright_stops,
            dark_stops: self.dark_stops,
            constrict_seconds: self.constrict_seconds,
            dilate_seconds: self.dilate_seconds,
        }
    }

    /// Full OSC address for the avatar parameter.
    pub fn address(&self) -> String {
        format!("/avatar/parameters/{}", self.parameter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_tuning_we_designed_for() {
        let c = Config::default();
        assert_eq!(c.parameter, "PupilSize");
        assert_eq!(c.osc_port, 9000);
        assert!(
            c.constrict_seconds < c.dilate_seconds,
            "constriction is faster"
        );
    }

    #[test]
    fn the_shipped_template_parses_into_those_defaults() {
        // If the template and the defaults ever drift, a first-run user gets
        // different behaviour from a no-config user. Catch it here.
        let parsed: Config = toml::from_str(TEMPLATE).expect("template must parse");
        let d = Config::default();
        assert_eq!(parsed.parameter, d.parameter);
        assert_eq!(parsed.osc_port, d.osc_port);
        assert_eq!(parsed.window_title, d.window_title);
        assert_eq!(parsed.capture_fps, d.capture_fps);
        assert_eq!(parsed.center_weight, d.center_weight);
        assert_eq!(parsed.bright_stops, d.bright_stops);
        assert_eq!(parsed.dark_stops, d.dark_stops);
        assert_eq!(parsed.constrict_seconds, d.constrict_seconds);
        assert_eq!(parsed.dilate_seconds, d.dilate_seconds);
    }

    #[test]
    fn missing_fields_fall_back() {
        let c: Config = toml::from_str(r#"parameter = "Eyes""#).unwrap();
        assert_eq!(c.parameter, "Eyes");
        assert_eq!(c.osc_port, Config::default().osc_port);
    }

    #[test]
    fn hand_edited_nonsense_is_clamped() {
        let c = Config {
            center_weight: 5.0,
            capture_fps: 0,
            constrict_seconds: -3.0,
            ..Config::default()
        }
        .validated();
        assert_eq!(c.center_weight, 1.0);
        assert_eq!(c.capture_fps, 1);
        assert_eq!(c.constrict_seconds, 0.0);
    }

    #[test]
    fn a_collapsed_brightness_range_is_replaced() {
        let c = Config {
            bright_stops: -5.0,
            dark_stops: -5.0,
            ..Config::default()
        }
        .validated();
        assert!((c.dark_stops - c.bright_stops).abs() > 0.1);
    }

    #[test]
    fn address_is_the_vrchat_parameter_path() {
        let c = Config::default();
        assert_eq!(c.address(), "/avatar/parameters/PupilSize");
    }
}
