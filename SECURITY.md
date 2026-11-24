# Security

## Reporting a problem

Please report security problems privately, not in a public issue:

1. Open the [Security tab](https://github.com/Leeuum/primitive-gpu/security) of this repository.
2. Click "Report a vulnerability" and describe the problem and how to reproduce it.

This is a spare-time project with one maintainer. Expect a first reply within two weeks. If the problem is confirmed, it is fixed in the next release, and the release notes credit you unless you ask not to be named.

## What counts

Only the newest release gets fixes.

Examples of what to report:

- A way for another device to read or write files through `primitive-gui` outside its upload, temp and render folders, or to see any file through its file browser other than the pictures and videos in the home folder (hidden files and folders are never shown).
- A picture or video file that makes either program run code it should not.
- A problem in one of the libraries or the ffmpeg build shipped in the download.

## Known limits (by design)

`primitive-gui` has no password. By default it listens on this computer only (`127.0.0.1`). With the `-lan` flag it listens on all network interfaces, so any device on the same network can use it. This is how the page works from a phone. Use `-lan` only on a network you trust, and never expose its port (8640 by default) to the internet.
