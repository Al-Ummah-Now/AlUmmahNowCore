# Studio media engine (`aun-media`)

Epic 30's shared logic. Hardware encoding and decoding stay in the app repos; everything that must
behave the same on every platform lives here.

## Encoding

A preset and a probed source become a `JobPlan`: a typed DAG that serialises to JSON and is validated
on construction. The planner picks hardware when `BackendCapabilities` says it fits and software
otherwise, and fails with `Unsupported` when neither can do the job.

`JobRunner` splits the plan into **stages**. A stage is one encoded output (or sink) plus every raw
node feeding it, so a native backend can run a whole stage as one pipeline. Encoded streams are handed
between stages as files in the job's work directory.

### Implementing a backend

Implement `EncodeBackend`. `execute_stage` is called on a blocking worker, one stage at a time per
job, and may block until the stage is done. It must:

- call `reporter.checkpoint()` often and propagate its `Cancelled` error promptly; it blocks while
  the job is paused;
- report progress in `0.0..=1.0`;
- write sink outputs atomically, so a cancelled job leaves no partial file;
- normalise loudness as heard in `output_channels`, so an upmixed mono source lands on target.

Validate it with `run_conformance`, passing an H.264 + audio fixture as `video_fixture` because only
the software backend understands `synthetic:` sources.

`SoftwareBackend` is a reference implementation for tests. Its audio processing is real, its video is
simulated, and its container is private: it must never produce a file meant for viewers.

## Edit projects

`schemas/edl/v1.json` is the published format. `load_project` migrates any known version and then
validates the result. To change the format: bump `CURRENT_SCHEMA_VERSION`, add a `Migration`, add
`schemas/edl/vN.json`, and add a fixture of the old version.

Edits (`split_clip`, `trim_clip`, `ripple_delete_clip`, `ripple_delete_range`) run on a copy and commit
only if the result validates.

### Render planning rules

`plan_render` cuts the video timeline at every clip, title, burned-in caption and transition boundary.

A segment is stream-copied only when all of these hold: one clip with no effects (no speed change,
transition or burned-in text), the same codec, size and frame rate as the output, known keyframes, and
a start on a keyframe. If only the start is off a keyframe, the head is re-encoded up to the next
keyframe and the rest is copied. The end of a copied segment need not be on a keyframe.

Adjacent re-encoded segments are joined before encoding, so they become one encode.

Audio is copied only when the clips abut with no gaps, have no volume or speed changes, match the
output format, and no loudness target is set. Otherwise every audible clip is mixed and re-encoded,
including clips on lower video tracks that are hidden in the picture.

Only the topmost video clip is shown, since the model has no opacity. A transition stays whole even if
a title starts inside it, and titles or captions that begin after the last video clip are not drawn.

Trimming a clip that belongs to a transition removes the transition, so a trim that leaves the two
clips overlapping is rejected until the overlap is resolved.
