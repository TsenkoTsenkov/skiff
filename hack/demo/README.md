# Demo animation

`docs/demo/skiff-demo.gif` (at the top of the README) and the sharper
`docs/demo/skiff-demo.mp4` it links to are a recording of the real app, not a
mock-up. Copies of both are in `site/img/` for the landing page.

## How it is made

1. `crates/skiff/tests/demo.rs` is an opt-in (`#[ignore]`) test. Like the UI
   tests, it runs the app headlessly with egui_kittest against the `demo`
   kwok cluster, at 1280x800 in the dark theme, and walks through a scripted
   tour with real key presses and pointer events. Each step is saved as a
   numbered PNG in `target/demo/frames/`, with a `manifest.jsonl` that says
   how long each frame stays, where the pointer is and which caption applies.
2. `render.py` (python3, Pillow, ffmpeg) turns the frames into the GIF and
   the MP4. It adds only what is not part of the app's own picture: the
   caption bar under the window, the pointer and the ring where it clicks
   (the headless app has no cursor of its own), and the cross-fades between
   scenes.

The tour, about 43 seconds: the overview with what is broken and the firing
alerts; the palette (Ctrl+K) to the pods and the `/` filter; a pod's details
and its Metrics tab; logs following in the dock; a shell in the container;
the cluster made read-only and a greyed-out Delete saying why; settings.

## Re-rendering

```sh
hack/cluster-up.sh             # once: the test clusters, the log simulator, the fake Prometheus
hack/demo/render.sh            # record, render, copy to docs/demo/ and site/img/
hack/demo/render.sh --render   # only render again, from the frames in target/demo/frames/
```

Needs what the UI tests need (a Vulkan driver; `mesa-vulkan-drivers` on a
machine without a GPU), plus `python3` with Pillow and `ffmpeg`. `render.sh`
also writes `target/demo/contact-sheet.png`, 24 evenly spaced frames of the
finished GIF: look at it before committing. `render.py --help` lists the
options (GIF width and number of colours).

To change the tour, edit `record_demo` in `crates/skiff/tests/demo.rs`: the
captions are the `rec.scene(...)` lines and the holds are the milliseconds
given to `rec.frame`.

## What to know about the pictures

- Everything shown comes from the synthetic `demo` cluster seeded by
  `hack/seed/`. The metrics and alerts are made up by
  `hack/fake-prometheus.py`, the log lines by `hack/logsim.sh`.
- kwok pods are not real containers: the test bed runs the shell on the
  recording computer as `nobody` in `/tmp`, so the prompt shows that
  computer's host name. bash prints complaints about this when it starts;
  the test types `clear` before it records the shell, and drags the dock
  taller a second time, both off camera.
- The log lines and the pod counts differ from one recording to the next,
  because the test bed keeps changing the cluster.
- The GIF uses one 256-colour palette at the recorded size and stores a frame
  only when the picture changes (12 steps a second while something moves).
  The MP4 is H.264, yuv420p, 30 frames a second, with the index at the front.
