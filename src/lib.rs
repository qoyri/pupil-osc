// SPDX-License-Identifier: MIT
//
// Qoyri pupil-osc — scene brightness to avatar pupil, over OSC.
//
// The chain is: capture a frame of the VRChat window -> reduce it to one
// centre-weighted luminance -> run that through a logarithmic, asymmetrically
// damped response -> send the resulting -1..1 float to VRChat.
//
// Everything except the capture itself is platform-free and unit-tested, which
// is deliberate: the Windows layer is the one piece that cannot be exercised
// off Windows, so it is kept as thin as it can be.

#[cfg(target_os = "windows")]
pub mod capture;
pub mod config;
pub mod luminance;
pub mod osc;
pub mod pupil;
