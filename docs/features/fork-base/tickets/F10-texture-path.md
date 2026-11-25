## F10: Texture uploads and detiling
**Status:** TO-DO
**What to build:** The textures step (120-280 ms per frame on the fork) cut down: what is detiled and hashed per frame and why, with detiling moved to the GPU (a compute pass reading tiled bytes from a buffer, or from the in-place heap of F08) and re-uploads only of what changed.
**Blocked by:** F07
**Requirements addressed:** 30 fps
- [ ] The textures step attributed (bytes, counts, hashing, detiling, uploads).
- [ ] A GPU detiler for the modes the title uses, tested against the CPU detiler.
