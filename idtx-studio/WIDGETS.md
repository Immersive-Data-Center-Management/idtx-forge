# Authored radial and polygon widgets

These are core USD/runtime features. The Poser/Conductor outline editor is deferred.
The existing widget rasterizer is also the preview renderer; its shape computation
lives in `idtx-scene::widget_layout`, independent of the rendering backend.

## Radial containers

Use `IdtxWidgetContainer` with `uniform token idtx:widget:layout = "radial"`.
Its immediate `IdtxWidgetButton` children are slots, sorted by `idtx:widget:order`
then panel-relative namespace path. Disabled buttons keep their slots and cannot
activate. Labels, colors and enabled state remain ordinary button properties.

| Container attribute (`idtx:widget:radial:` prefix) | Default | Meaning |
| --- | --- | --- |
| `center` (`float2`) | `(0.5, 0.5)` | Normalized center in the container's allocated bounds |
| `innerRadius` / `outerRadius` (`float`) | `0.15` / `0.5` | Fractions of the smaller bound dimension; `0 <= inner < outer <= 0.5` |
| `startAngle` (`float`) | `0` | Degrees, zero up and positive clockwise; beginning of the first slot |
| `sweepAngle` (`float`) | `360` | Positive sweep, at most 360 degrees |
| `gapAngle` (`float`) | `2` | Nonnegative angular gap at the inner ring, converted to constant-width, parallel-sided spacing |
| `deadZone` (`float`) | `0.25` | Stick magnitude threshold, in `[0,1)` |
| `hysteresisAngle` (`float`) | `4` | Boundary hysteresis for the previously focused slot, `[0,45]` degrees |

Angles are start-inclusive/end-exclusive; radii are inner-inclusive/outer-exclusive.
The gap keeps its inner-ring width all the way to the outer ring. Its width is
`2 * innerRadius * sin(min(gapAngle, 180) / 2)` (angle in degrees, radius in layout
pixels); with `innerRadius = 0`, `outerRadius` is the reference instead. Stick
navigation evaluates the resulting shape at the label ring.
The exact center never selects. Invalid radial settings produce no sectors.
The retained hierarchy allocates row/column/grid/stack bounds around radial
subtrees. Rectangle-only assets keep their existing layout and spacing rules.

## Polygon panel surfaces

All properties are on `IdtxWidgetPanel`:

```usda
float2[] idtx:widget:background:vertices = [(0, 0), (1, 0), (0, 1)]
int[] idtx:widget:background:triangleIndices = [0, 1, 2]
color3f idtx:widget:background:color = (0.1, 0.2, 0.3)
float idtx:widget:background:opacity = 0.9
```

Vertices use normalized panel coordinates, top-left origin. Indices are zero-based
triangle triples; either winding is accepted. Author triangulation explicitly to
represent concavities, disconnected regions or holes. All panel content is clipped
to the triangle union. Both arrays empty retain the legacy rectangular surface.
Invalid indices, non-finite/out-of-range vertices, degenerate triangles, or limits
over 4096 vertices / 8192 triangles produce an empty surface.

`background:opacity = 0` removes only the background, making radial corners,
gaps and the dead center transparent. The panel's existing `idtx:widget:opacity`
multiplies the final composition; element opacity multiplies through its container
ancestry. Effective USD visibility removes hidden descendants from drawing/picking.

Static textures are content-hash cached. Controller movement changes the panel
transform. Focus highlighting updates shader uniforms and reuses the texture.

Numeric `IdtxWidgetFieldFloat` / `IdtxWidgetFieldDouble` readouts support
`idtx:widget:precision` from **0 to 16** decimal places. Missing or negative values
use the existing default of **3**; larger values clamp to **16**, with a diagnostic.
Clamping affects display text only and does not rewrite authored USD values.

## Interaction and ownership

- `ctx.widget().on_click(path, handler)` addresses an authored child.
- `ctx.widget().on_panel_click(handler)` receives blank covered areas and
  unhandled children, including labels. A registered child handler consumes first.
  `WidgetEventKind::PanelClick` distinguishes background targets; no sentinel
  child name is used.
- `ctx.widgets().handle_from(hit, source, click_action)` returns separate
  `occluded` and `handled` flags. World interaction must stop on `occluded`, even
  without a registered callback. The legacy `handle` boolean now means occluded.
- `navigate(panel, container, previous_path, stick, step)` uses the computed
  layout. `step=0` uses stick magnitude/hysteresis; `+1/-1` cycles for keyboard.
  `highlight(panel, path)` is transient. `activate(panel, path, source, confirm)`
  requires a confirmation edge and validates current visibility, enabled state,
  membership and clipping. Queued events are revalidated at delivery.
- `WidgetClick::source()` preserves pointer identity. An ambiguous aggregated
  action reports `Unspecified`; use `Action::from_source` for independent hand
  edges and values. The example defaults to the right controller.

Request `input::CaptureToken` in `on_input_capture(&mut self, ctx: &Ctx)`. All
such hooks run before input/update consumers; claims then arbitrate together by
descending priority, owner entity and behavior identity. Requests from ordinary
hooks take effect at the next pre-input boundary. A token is all-or-nothing for
its logical-action set. Conflicting aliases must be explicit: capture both
`menu.navigate` and `turn`, and both confirmation and world/widget click aliases.
New ownership guards held buttons until release. Releasing ownership suppresses
held button edges for other consumers until release as well.

Capture work receives at most half the aggregate frame budget. An overloaded
sweep rotates owners and yields the next frame to ordinary hooks/callbacks;
retained claims still commit before that frame's input is consumed.

Tokens are scoped by player and source. A null player resolves to the owner's
player root; if none exists it is a session-wide scope. Bind a `player`
relationship for panels under the level. Release/drop on close. The host also
retires claims on detach, target removal, hidden targets, tracking loss and stop.
`BehaviorHost::input_focus_owners()` exposes owner/action diagnostics.

Attachment uses existing transforms: author a panel below a tracked controller,
or bind the example's `controller` and `attachmentOffset` relationships. The
offset target's `parent_relative_matrix()` is interpreted in controller space.
The example cancels the USD/render unit and up-axis basis in the parent-relative
read and final placement, then writes with `stage_edit().set_transform()` in
authored USD units. Top-level panels use `scene().usd_to_world()` as their parent
frame. Controller pose/recenter updates come from the existing XR APIs.
Selecting-controller stick confirmation avoids the unusable geometry of aiming
that same controller's rigidly attached ray at its menu. Another hand can use
`handle_from` as an optional pointer mode.

Fixed and billboard panels use `resolve_widget_quad` for drawing and picking.
Both stereo eyes project one central-camera widget surface; they do not define
different logical hit planes.

## Examples and checks

`examples/widgets/radial_menu.usda` has three configurable actions;
`five_actions.usda` references it, adds two buttons and disables one. Actions,
priorities and aliases are authored in behavior context; event handlers live in
`menu.rs`. `polygon_message.usda` / `message.rs` demonstrate a dismissible concave
surface. For keyboard simulation set context `pointer` to `1` (desktop); `3` is
right controller. Bind the example's `coordinator` relationship to receive events.
The supplied mappings cover keyboard and Quest controllers; Vision Pro requires
project-authored hand/gaze opening, navigation and confirmation bindings.

Polygon coverage is cached by geometry and resolution, independently of widget
text/value changes. Duplicate triangles are folded; outlines are limited to
1,048,576 triangle scanlines at the maximum 4096-pixel axis. Over-budget outlines
are diagnosed and become empty for both drawing and picking.

`tests/fixtures/usd/widgets/radial_and_polygon.usda` composes these assets under a
translated/rotated parent. Tests cover pixel/hit samples, nested menus, disabled
slots, hysteresis, ownership arbitration, opening/release edges, USD hydration,
undo, and packaged save/reopen.
