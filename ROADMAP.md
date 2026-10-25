# Roadmap

What each coming release is for, in plain words. One release at a time; each one ends with a test by
someone who has nothing but this repository's URL. The order of 0.4 to 0.6 can still change.
[CHANGELOG.md](CHANGELOG.md) says what each finished release brought.

## Done

- **0.1 — a fast, correct command-line renderer.** GPU deep zooms with automatic precision down to
  1e-100 and beyond, checked against exact calculations, crash-safe resume, signed releases for
  Windows, Linux and Docker.
- **0.2 — keyframe videos.** The zoom is rendered once and the video assembled from it: about six
  times less GPU work, antialiased frames, no brightness pumping, and new colours or speed in
  seconds.
- **0.3 — the app.** A page in your browser, on Windows (double-click the exe) and on a server (one
  compose file): pick a place, see the time and disk it will take, watch it render with snapshots
  to look back over, recolour it with a palette editor that previews instantly, make the video.
  Letterbox bars (blurred, solid or transparent), pausing while a game is full screen, a tray icon,
  Discord and upload notifications.

## Next

- **0.4 — the finder.** Ask it to find places for you: "one like this but deeper" within a time
  limit, or a new place with a name, a minimum depth and how busy it should be. The reference
  calculation moves to our own code, replacing the GMP/MPFR libraries.

## Later

- **0.5 — Julia sets and sharing.** A second family of fractals; share places and colour schemes as
  copy-paste codes, then through a community list with search, stars and download counts, all
  hosted on GitHub.
- **0.6 — the Burning Ship family.** Burning Ship and about nine relatives, added together through
  one shared building block.
- **1.0 — installer, updates and a guide.** An installer and a portable version from one program
  that update themselves safely, a code-signed exe, and a how-it-works site with simple
  explanations and technical detail you can open.
