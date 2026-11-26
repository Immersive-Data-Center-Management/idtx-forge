# Animation and event-routing extension

Copy the assets/, behaviors/ and extensions/ files from this directory into a
writable Signal Room project. Do not replace the base project or input map.
Enable extensions/animation-routing.usda as a sublayer of Hub or Lab, following
the handbook chapter tutorial/06-animation-and-routing.md. That page also
contains every file as a complete listing.

This is an opt-in extension, not a separate project with its own project.toml.
It needs the current bundled SDK and compatible Runtime: relationship targets,
named authored clip declarations, playback status/release and animation events.
Open the amended level in packaged Conductor, check both new bindings, then use
Preview > Preview in Desktop. The existing Signal Room pointer operates the
new panel. Build the full experience to verify the extension after room travel.

The orange exhibit idles. React / Restart runs a one-second rise/spin and returns
to idle on its completion event. Hold current pose pauses the currently owned
clip. Release to timeline returns sampled content to the containing USD clock.
The panel displays settled playback status rather than treating a queued
command as proof that it applied.
