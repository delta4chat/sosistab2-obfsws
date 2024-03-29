# [Websocket](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) (with or without TLS) pluggable-transport implementation for [sosistab2](https://github.com/geph-official/sosistab2).

# TODO
- [ ] Try to solve "flow cut-offs" in BUG.txt
- [ ] Try to solve the TCP-over-TCP problem in sosostab2. should be to add a trait type called "Reliable Pipe" to tell sosistab2 to turn off retransmission and reordering.

# Background
Most CDNs, "serverless", or web app hosting platforms [does not allow "unknown traffic"](https://quicwg.org/ops-drafts/draft-ietf-quic-applicability.html#name-the-necessity-of-fallback) to pass through their load balancing facilities, and worse, their server network environments are basically firewalled or behind a [Symmetric NAT](https://www.zerotier.com/blog/the-state-of-nat-traversal/), so users can't simply set up a service that can be accessed from the public internet (unless through these "reverse-proxy" set up by hosting providers). 

  1. _For example, if you have a web server listening `http://0.0.0.0:8080/` on the machine provided by your hosting platform, then you can use `https://your-app-name.hosting-platform.com/` to access your Web service, but not any other method. (even P2P applications such as IPFS, which use UDP and STUN, are unable to implement NAT Traversal in this network environment)._

For example, back in the day, when `heroku.com` still offered free plans: there were many people from mainland China using [v2ray ws+tls](https://www.v2fly.org/v5/config/stream/websocket.html) servers on their platforms to bypass [GFW internet censorship](https://en.wikipedia.org/wiki/Great_Firewall), which is actually a proxy protocol delivered over websockets (after all, any unknown traffic like [shadowsocks](https://shadowsocks.org/) can't pass through their web reverse proxy).

