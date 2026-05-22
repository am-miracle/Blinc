# Transparent window — regression test for GH #34.

no-web

Linux Mesa surfaces only expose `[Opaque, PreMultiplied]`; the
previous hardcoded `PostMultiplied` selection panicked on Linux
during `Surface::configure`. The fix queries the surface's actual
supported alpha modes (`windowed.rs::pick_alpha_mode`) and falls
back to `PreMultiplied` when `PostMultiplied` isn't there.

What you should see when this runs:
- A normal window with a translucent rounded card.
- The desktop behind the window shows through the card's
  `rgba(_, _, _, 0.78)` background.
- No wgpu validation panic at startup.

<iframe
  src="../../examples/transparent_window/index.html"
  width="100%"
  height="560"
  loading="lazy"
  style="border:1px solid #45475a;border-radius:8px;background:#181825;"
  title="Blinc transparent_window example"
></iframe>

> **Tip:** Some demos are best viewed in a full browser window. Click "Open in a new tab" below for the full experience.

[Open in a new tab](../../examples/transparent_window/index.html) · [View source on GitHub](https://github.com/project-blinc/Blinc/blob/main/examples/blinc_app_examples/examples/transparent_window.rs)
