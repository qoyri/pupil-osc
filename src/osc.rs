// SPDX-License-Identifier: MIT
//
// One outbound float to VRChat. Mirrors desktop-app's sender rather than sharing
// it: the useful part is a dozen lines of rosc, and a shared crate would cost
// more in plumbing than it saves. If a third tool needs this, extract it then.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use rosc::{encoder, OscMessage, OscPacket, OscType};

/// Only send when the value actually moved. Below this the avatar cannot show a
/// difference anyway, and VRChat is happier without the traffic.
const EPSILON: f32 = 0.002;

/// Repeat the current value at least this often, so an avatar that just loaded
/// picks up the pupil instead of sitting at its default until the light changes.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(1);

pub struct PupilSender {
    socket: UdpSocket,
    target: SocketAddrV4,
    address: String,
    last_sent: Option<f32>,
    last_time: Option<std::time::Instant>,
}

impl PupilSender {
    /// Binds an ephemeral local port aimed at VRChat on loopback.
    pub fn new(port: u16, address: String) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        Ok(Self {
            socket,
            target: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
            address,
            last_sent: None,
            last_time: None,
        })
    }

    /// Sends `value` if it moved enough or the heartbeat is due. Returns whether
    /// a packet actually went out, which is what the console readout reports.
    pub fn send(&mut self, value: f32, now: std::time::Instant) -> io::Result<bool> {
        let due = match (self.last_sent, self.last_time) {
            (Some(previous), Some(when)) => {
                (value - previous).abs() >= EPSILON || now.duration_since(when) >= HEARTBEAT
            }
            _ => true,
        };
        if !due {
            return Ok(false);
        }

        let packet = OscPacket::Message(OscMessage {
            addr: self.address.clone(),
            args: vec![OscType::Float(value)],
        });
        let bytes = encoder::encode(&packet)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        self.socket.send_to(&bytes, self.target)?;

        self.last_sent = Some(value);
        self.last_time = Some(now);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Binds a receiver and a sender aimed at it.
    fn pair() -> (UdpSocket, PupilSender) {
        let receiver = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = receiver.local_addr().unwrap().port();
        receiver.set_nonblocking(true).unwrap();
        let sender = PupilSender::new(port, "/avatar/parameters/PupilSize".into()).unwrap();
        (receiver, sender)
    }

    fn recv_float(socket: &UdpSocket) -> Option<(String, f32)> {
        let mut buf = [0u8; 1024];
        let n = socket.recv(&mut buf).ok()?;
        match rosc::decoder::decode_udp(&buf[..n]).ok()?.1 {
            OscPacket::Message(m) => match m.args.first() {
                Some(OscType::Float(f)) => Some((m.addr, *f)),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn sends_the_parameter_as_a_float() {
        let (rx, mut tx) = pair();
        let now = Instant::now();
        assert!(tx.send(-0.5, now).unwrap());

        let (addr, value) = recv_float(&rx).expect("a packet");
        assert_eq!(addr, "/avatar/parameters/PupilSize");
        assert!((value - -0.5).abs() < 1e-6);
    }

    #[test]
    fn an_unchanged_value_is_not_resent_immediately() {
        let (rx, mut tx) = pair();
        let now = Instant::now();
        assert!(tx.send(0.25, now).unwrap());
        let _ = recv_float(&rx);

        // Same value a moment later: nothing on the wire.
        assert!(!tx.send(0.25, now + Duration::from_millis(50)).unwrap());
        assert!(recv_float(&rx).is_none());
    }

    #[test]
    fn a_real_move_goes_out() {
        let (rx, mut tx) = pair();
        let now = Instant::now();
        tx.send(0.0, now).unwrap();
        let _ = recv_float(&rx);

        assert!(tx.send(0.5, now + Duration::from_millis(50)).unwrap());
        assert_eq!(recv_float(&rx).map(|(_, v)| v), Some(0.5));
    }

    #[test]
    fn the_heartbeat_repeats_a_steady_value() {
        let (rx, mut tx) = pair();
        let now = Instant::now();
        tx.send(0.25, now).unwrap();
        let _ = recv_float(&rx);

        // Unchanged, but past the heartbeat: an avatar that just loaded needs it.
        assert!(tx.send(0.25, now + Duration::from_secs(2)).unwrap());
        assert_eq!(recv_float(&rx).map(|(_, v)| v), Some(0.25));
    }
}
