# Previous / Legacy Collaboration Transport Modules (Archived)

These files are the previous pre-Iroh custom networking modules from Lumen 14.8, archived here for reference rather than deleted:

- `net.rs` — Legacy TCP/WebSocket + UDP LAN beacon transport
- `upnp.rs` — Legacy UPnP-IGDv1 IPv4 port mapping and UPnP-IGDv2 IPv6 firewall pinhole client
- `punch.rs` — Legacy STUN + UDP hole punching + custom RUDP
- `turn.rs` & `lumen_turn.rs` — Legacy self-hosted UDP TURN relay server
- `dht.rs` — Legacy BitTorrent Mainline DHT peer announcer/lookup
- `relay.rs` — Legacy HTTP long-poll relay transport
- `webnet.rs` — Legacy browser WebSocket transport
- `rtc.rs` — Legacy browser WebRTC DataChannel transport

All active networking is now unified in `../iroh_transport.rs` (Iroh QUIC P2P + automatic hole punching + n0 public relays) alongside `../mdns.rs` for zero-config offline LAN discovery.
