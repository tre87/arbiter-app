//! Wake-on-LAN: the magic packet that wakes a machine whose network card listens for
//! it, sent as a UDP broadcast on the local network.
//!
//! The packet is six bytes of `0xFF` followed by the target's MAC address sixteen
//! times; the card matches that pattern anywhere in the frame. It goes to the limited
//! broadcast address on UDP port 9 (the discard port, the convention `wakeonlan` and
//! `etherwake` follow) and port 7 (echo, which some cards and routers are set up for),
//! from whatever interface carries the default route. That reaches machines on the
//! same LAN or Wi-Fi; a machine on another subnet or across a VPN would need a directed
//! broadcast, which this does not attempt.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

/// A saved target: what to call it, and its MAC as the user typed it (see `parse_mac`
/// for the forms accepted).
pub const PORTS: [u16; 2] = [9, 7];

/// A MAC address from any of its usual spellings: pairs separated by `:` or `-`,
/// Cisco's dotted quads (`aabb.ccdd.eeff`), or twelve bare hex digits, any case.
/// `None` for anything else, including the all-zero and broadcast addresses, which
/// no card answers to.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let digits: String = text.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();
    if digits.len() != 12 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    if mac == [0; 6] || mac == [0xFF; 6] {
        return None;
    }
    Some(mac)
}

/// The canonical spelling, `aa:bb:cc:dd:ee:ff`.
pub fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// The magic packet for `mac`: 6 × `0xFF`, then the address 16 times (102 bytes).
pub fn magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut packet = [0xFFu8; 102];
    for i in 0..16 {
        packet[6 + i * 6..12 + i * 6].copy_from_slice(mac);
    }
    packet
}

/// Send the magic packet for `mac` as a broadcast on every port in `PORTS`. Succeeds
/// when every send was accepted by the network stack, which is all that can be known:
/// UDP has no acknowledgement, and a sleeping machine sends nothing back.
pub fn send(mac: &[u8; 6]) -> io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    for port in PORTS {
        socket.send_to(&packet, SocketAddrV4::new(Ipv4Addr::BROADCAST, port))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_is_read_in_its_usual_spellings_and_nothing_else() {
        let want = Some([0x00, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E]);
        assert_eq!(parse_mac("00:1A:2B:3C:4D:5E"), want);
        assert_eq!(parse_mac("00-1a-2b-3c-4d-5e"), want);
        assert_eq!(parse_mac("001a.2b3c.4d5e"), want);
        assert_eq!(parse_mac("001A2B3C4D5E"), want);
        assert_eq!(parse_mac("  00 1a 2b 3c 4d 5e "), want);
        assert_eq!(parse_mac("00:1A:2B:3C:4D"), None, "too short");
        assert_eq!(parse_mac("00:1A:2B:3C:4D:5E:6F"), None, "too long");
        assert_eq!(parse_mac("00:1G:2B:3C:4D:5E"), None, "not hex");
        assert_eq!(parse_mac("00:00:00:00:00:00"), None, "nobody's address");
        assert_eq!(parse_mac("ff:ff:ff:ff:ff:ff"), None, "everybody's address");
        assert_eq!(parse_mac(""), None);
        assert_eq!(format_mac(&want.unwrap()), "00:1a:2b:3c:4d:5e");
    }

    #[test]
    fn the_magic_packet_is_six_ffs_then_the_mac_sixteen_times() {
        let mac = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let packet = magic_packet(&mac);
        assert_eq!(packet.len(), 102);
        assert!(packet[..6].iter().all(|&b| b == 0xFF));
        for i in 0..16 {
            assert_eq!(&packet[6 + i * 6..12 + i * 6], &mac, "repeat {i}");
        }
    }

    // The network stack accepts a broadcast from an unbound socket on any machine with
    // an interface up; the packet goes to no one in particular.
    #[test]
    fn a_broadcast_send_is_accepted() {
        send(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).expect("send accepted");
    }
}
