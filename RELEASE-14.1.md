# Lumen 14.1

Collaboration hardening pass — see [COLLAB.md §8](COLLAB.md) for the findings table.

* Protocol 2: Noise XX encryption of every frame (LAN, browser WebSocket, relay), deflate compression, channel-bound token proof,
  mutual host/joiner authentication, 4-digit security code on approval cards.
* Resource limits against strangers (connections, approvals, queues, message sizes, flood control), verified media transfers.
* Relay (`/api/collab` on the site): per-device session keys, caps, rate limit, TTL.
* Android: `lumen://join` deep link, `MulticastLock`, no backup, no cleartext.
* Viewer role: typing / paste / cut / delete are blocked in the viewer's own client.
* Battery: adaptive socket polling and repaint timer.
