# Conductor widget editor — implementation plan

- **Status:** proposed implementation plan.
- **Baseline:** source review of `7bddaa4`, 2026-11-24.
- **Paths:** relative to `idtx-studio/`. Proposed APIs and attributes below are
  not claims of existing support.

## 1. Goal and release contract

Add a **Widget Editor** tab immediately beside **3D View** in Conductor. Opening
or activating a widget stage automatically selects that tab. The editor presents
the owning panel in a fixed, panel-local orthographic view, renders its content
through the runtime widget rasterizer, and authors USD widget properties through
the existing session and undo infrastructure.

The central implementation decision is to have **one resolved widget layout**
drive pixels, selection, gizmos, clipping, and movement constraints.

### Required behavior

| Requirement | Implementation contract |
| --- | --- |
| Automatic editor activation | Classify the active document using its live root layer's `customLayerData.idtx.stageKind`. The `widget` token activates the editor on stage open, document-tab activation, and a metadata transition into widget mode. |
| Orthographic presentation | Display panel-local XY with a fixed front-facing orientation, pan and zoom. Authoring navigation changes the canvas view state. |
| Additional center tab | Order: `3D View`, `Widget Editor`, `Material Canvas`, `Compute Graph` for a widget stage. A user can inspect the 3D tab; the next document activation selects Widget Editor again. |
| Panel and element inspectors | Dedicated contributions expose typed widget properties, defaults, units, authored opinions, and live feedback. |
| Picking | A click selects the actual composed USD element under the pointer, using final layout shapes, clipping, and reverse paint order. Empty covered panel space selects the panel. |
| Movement gizmo | Movable elements and containers have X, Y, and free-XY translation handles. Translation authors 2D layout attributes and stays within the owning panel. Managed children use the arrangement-aware rules in section 7.3; radial sectors are positioned by their container. |
| Panel types | New panels support **Rectangle** and **Circle**, selected by **Surface Shape** in the panel inspector. Shape controls the panel surface and clipping, independently of any child arrangement. |
| Container arrangements | **Free**, **Row**, **Column**, **Grid**, **Table**, **Stack**, and **Radial** belong to the container inspector's **Arrangement** control. They share `IdtxWidgetContainer`; Free and Table require version-2 schema/runtime additions. Changes between arrangements follow the conversion rules in section 5.6. |
| Scene-tree context menu | In a widget stage, its authoring entries are exclusively **Add Widget Element ▶** and **Remove Widget Element**, subject to valid parent/target rules. Add opens a filtered selection of concrete elements, including Field, Image, Button, Label, and Container presets; section 8.2 specifies the submenu. |
| Responsive authoring | Anchoring, percentage sizing, grid snapping, and text wrapping are included in the planned release, with their runtime dependencies described in chapter 9. |
| Persistence | Creation, property edits, movement, and removal support undo/redo and save/reopen, including packaged widget assets. |

Here, **stage selected** means activating a stage/document, including reopening
an already-open asset from the browser. Selecting a prim in the scene tree only
updates the selection and inspector. Automatic activation is event-driven, so it
does not interrupt a deliberate center-tab switch on every frame.

### Initial authoring scope

- Native `IdtxWidgetPanel` documents with the existing concrete element types:
  Container, Label, Button, Image, Slider, Toggle, and the four typed Fields.
- New direct panel children and children of Free containers use explicit,
  free-positioned layout boxes. Inserting into a managed container creates a
  flow child in its next slot/cell, with the movement rules in section 7.3.
- Row, Column, Grid, Table, Stack, and Radial are authored arrangements of child
  elements. A Table is an authored cell layout with shared tracks; data-driven
  row generation and interactive data-table features are follow-up work.
- New widget stages have one default/root panel. Existing documents with several
  panels use an active-panel selector and show one panel at a time.
- Radial containers support creation and editing of their button slots, geometry,
  and navigation settings. Existing polygon surfaces retain their rendering and
  inspection support. Free-form polygon outline editing is a follow-up feature.
- Widget editing is a Conductor capability. Runtime layout and rendering changes
  live in the shared crates so packaged experiences consume the same contract.

## 2. Current implementation and concrete gaps

These findings are from source inspection, rather than a runtime acceptance run.

| Area | Existing implementation | Work required |
| --- | --- | --- |
| Stage classification | [`read_stage_kind`](crates/idtx-usd/src/lib.rs) reads the root-layer dictionary. [`StageKind`](crates/idtx-project/src/lib.rs) currently contains Level, Asset, Assembly, Player, and Transition. Existing widget fixtures already use `stageKind = "widget"`. | Add `StageKind::Widget` and a live-stage root-tag accessor, then route document activation through that classification. |
| Center tabs | [`CenterTab` / `SessionTab`](crates/idtx-studio-shell/src/model.rs), [`tabs.rs`](crates/idtx-studio-shell/src/tabs.rs), and [`render.rs`](crates/idtx-studio-shell/src/render.rs) initialize/display Viewport, MaterialCanvas, and ComputeGraph. | Add widget canvas state, presentation data, texture lifetime, and activation handling. |
| Inspector | The [`inspector registry`](crates/idtx-editor-ui/src/inspector/mod.rs) supports contributions, property claims, and edit sinks. It has no widget-specific contributions. `inspector/widgets.rs` contains general inspector controls. | Add panel/common-element/type-specific sections and reusable two-component/percentage controls. |
| Typed authoring | [`PropertyValue`](crates/idtx-usd/src/lib.rs) supports `Int2` but lacks `Float2`, `FloatArray`, and `Float2Array`. [`AttributeValue`](crates/idtx-editor-model/src/lib.rs) also lacks `Int2`. | Complete the UI → shell → safe USD → C++ value path before implementing position, size, resolution, table tracks/cells, or outline controls. |
| Schema hierarchy | [`schema.usda`](crates/idtx-usd-extension/schema/schema.usda): Panel derives from Gprim; Element derives from Imageable. Elements have visibility and 2D position/order, but no independent xform or boundable geometry. | Add an explicit layout-box contract and derived picking geometry while preserving the element's 2D semantics. |
| Hydration | [`hydrator/widget.rs`](crates/idtx-scene/src/hydrator/widget.rs) aggregates descendants into `WidgetPanelComp`, retaining panel-relative paths, effective opacity, content, order, and radial settings. | Retain authored layout data, exact element identity, hierarchy, container boxes, and editor-selectable hidden elements separately from visible paint entries. |
| Layout | [`widget_layout.rs`](crates/idtx-scene/src/widget_layout.rs) shares shape computation with drawing and runtime hit-testing. Rectangle-only content uses legacy slot heuristics; the hierarchical path is selected when a radial container is present. `(0, 0)` position means unpositioned. | Version the layout contract; implement explicit size, origin-safe positioning, anchors, and consistent hierarchy independent of radial content. |
| Container arrangements | The schema and Rust `WidgetLayout` expose `row`, `column`, `grid`, `stack`, and `radial` on the same concrete Container type. Radial emits sectors only for immediate Button children. Free/Table tokens and table cell/track attributes do not exist. | Use one arrangement model and conditional inspector. Add Free/Table, deterministic flow order, and validated arrangement conversion; the USD audit and conversion matrix are in sections 5.5–5.6. |
| Rasterization | [`SolidWidgetRasterizer`](crates/idtx-render/src/widget_pass.rs) draws backgrounds, labels, controls, images, and radial sectors; polygon clipping and content-hash caching exist. | Chapter 9 identifies the remaining layout, typography, image-fit, and presentation work. |
| Editor picking | [`picking.rs`](crates/idtx-scene/src/picking.rs) requires `ViewportPickable + BoundsComp`, then performs broad/detail tests. Widget hydration supplies neither element picking proxies nor an element detail test. | Add a panel-scoped widget query using derived bounds and final widget shapes. |
| Runtime picking | [`scene_backend_impl.rs`](crates/idtx-studio-session/src/scene_backend_impl.rs) already intersects widget quads and resolves UVs, including shear and billboard handling. | Share the geometry helpers with the editor and consume the new layout snapshot. Keep editor selection independent of runtime event dispatch. |
| Native bounds | [`usd_shim.cpp`](crates/idtx-usd-sys/cxx/usd_shim.cpp) projects bounds for `UsdGeomBoundable` through `ComputeExtentFromPlugins`. | Adding an `extent` property to an Imageable element alone would not supply the current picker with usable bounds. See section 6. |
| Undo | [`authoring_ops.rs`](crates/idtx-studio-shell/src/authoring_ops.rs) captures exact root-property opinions; [`EditJournal`](crates/idtx-authoring/src/lib.rs) supports reversible property and structural changes. | Add grouped, target-aware property gestures for position/size/anchor conversions and cancellation. |
| HUD | [`WidgetRenderer::prepare`](crates/idtx-render/src/widget_pass.rs) explicitly skips `WidgetSpace::Hud`. [`hud.rs`](crates/idtx-render/src/hud.rs) implements anchored image HUDs via `IdtxHud`. | Implement screen-space widget composition and input mapping; changing texture resolution alone cannot deliver responsive HUD behavior. |

Useful existing assets and regressions:

- [`widget_panel_stage.usda`](tests/fixtures/usd/widgets/widget_panel_stage.usda)
  and [`widget_positioned.usda`](tests/fixtures/usd/widgets/widget_positioned.usda).
- [`radial_menu.usda`](examples/widgets/radial_menu.usda),
  [`five_actions.usda`](examples/widgets/five_actions.usda), and
  [`polygon_message.usda`](examples/widgets/polygon_message.usda).
- [`widget_schema.rs`](crates/idtx-usd-extension/tests/widget_schema.rs),
  [`widget_shapes.rs`](crates/idtx-scene/tests/widget_shapes.rs),
  [`widget_render.rs`](crates/idtx-render/tests/widget_render.rs), and
  [`behavior_widget.rs`](crates/idtx-studio-session/tests/behavior_widget.rs).

## 3. Architecture and ownership

```text
Live USD stage + authored widget properties
                  |
        WidgetSchemaHydrator
                  |
     Authored panel/element hierarchy
                  |
   Shared layout resolver + layout context
                  |
          WidgetLayoutSnapshot
          /        |          \
  Rasterizer   Picking      Bounds/constraints
      |            |              |
  Texture cache    +---- actual USD selection
      |                           |
  World / editor / HUD       Gizmo + inspector
  presentation                    |
                          Undoable USD edits
```

### 3.1 Shared resolved layout

Introduce an immutable `WidgetLayoutSnapshot` in `idtx-scene`, computed from a
panel's authored data and a `WidgetLayoutContext`. It should carry:

- Panel identity, stage generation, logical dimensions, shape, and layout revision.
- Each element's composed `PrimId`/path and owning-panel identity; resolve entity
  handles within the current stage generation.
- Parent/container indices, allocated box, final floating-point layout box,
  content box, clip chain, and exact hit/paint shape.
- Arrangement, flow sequence, and slot/cell membership, including radial sector
  and table cell data used by inspector overlays and conversion validation.
- Stable paint order and element kind; retain container boxes for selection.
- Effective visibility/opacity and interaction state, with editor eligibility
  represented separately from runtime activation eligibility.
- Read-only derived bounds and validation results, including overflow or missing
  asset/intrinsic-size information.

Preserve subpixel layout values. Convert logical coordinates to texel samples at
the raster boundary, using a documented edge/pixel-center convention. Quantizing
layout to integer texture coordinates before dragging would introduce zoom- and
resolution-dependent movement.

The resolver belongs in `idtx-scene`; font/image measurements reach it through a
small measurement interface or immutable measurement records. This maintains the
existing dependency direction: `idtx-render` consumes `idtx-scene`.

### 3.2 Editor state and presentation

Add per-document `WidgetEditorState` containing:

```text
active_panel_path
pan / zoom
preview_logical_size / presentation preset
grid spacing / visibility / snapping
outline and hidden-element display options
hover target
active gesture: stage generation, panel, element, target layer,
                captured authored opinions, pointer-down mapping
```

Keep selected prim identity in the existing session/scene-tree selection path.
`ActiveTabData` receives an immutable `WidgetEditorView` snapshot and presentation
texture; the pure egui canvas emits intents, following the existing shell pattern.

All intents and state write-back must address the originating `tab_id`. In
particular, switching documents during an egui frame must not write the previous
document's canvas/gesture state into the newly active tab.

### 3.3 Shared rasterization, separate presentation contexts

Factor the current widget renderer into:

1. Resolve/obtain the layout snapshot.
2. Rasterize and cache the panel texture.
3. Present that texture as a world quad, orthographic editor panel, or HUD quad.

Add a session-facing widget preview API and lazily allocated preview target.
The editor should request the active panel's texture through this API rather than
accessing private renderer caches or rebuilding the panel with egui controls.
The preview uses canonical panel placement and its own orthographic mapping.

Use the same asset reader, font resolution, clipping, and color/alpha conversion
as the runtime path. Release registrations/textures when a panel or document is
closed; key caches by document generation as well as panel identity.

## 4. Stage detection, activation, and creation

### 4.1 Root-layer classification

Continue the existing metadata convention:

```usda
customLayerData = {
    dictionary idtx = {
        token stageKind = "widget"
        string schemaVersion = "1"
        string displayName = "Status Panel"
    }
}
```

Implementation steps:

1. Add `Widget` to `idtx_project::StageKind`, including token parsing, labels,
   browser classification, and round-trip tests.
2. Add a live `StageService::root_stage_kind()` accessor returning `StageKindTag`.
   Factor the existing dictionary decoding so file-based browser classification
   and the live-stage accessor accept both token and string values.
3. Read the actual stage root layer, including an opened package's root layer.
   Referenced assets and sublayers carrying widget metadata do not classify their
   containing level as a widget document.
4. Cache the result in the session with metadata invalidation. Refresh after open,
   root metadata edits, undo/redo, layer reload, and relevant Sdf layer notices.
   The current open-time `StageMetadata` cache alone is insufficient for this.
5. Preserve other keys inside the `idtx` dictionary when changing the stage tag.
   The current setter replaces that dictionary, so extend its merge behavior
   before using it for existing documents.

File names, folder names, selected prim types, and the mere presence of a widget
panel are not classification inputs.

### 4.2 One activation entry point

Centralize document activation in a helper used by:

- Completion of `poll_loading_tabs`, including reload.
- `ShellAction::ActivateTab`.
- Asset-browser/level-browser paths that reactivate an existing document.
- Project startup/default-stage loading and metadata transitions.

For Conductor + widget kind, make `CenterTab::WidgetEditor` available and select
it at activation. Cancel a gesture before leaving its document. Preserve pan,
zoom, and active-panel state when revisiting that document.

When metadata changes away from widget kind, remove the widget-editor tab and
select 3D View if necessary. An unknown layout version can still open the editor
with diagnostics and inspectable content; document classification stays rooted
in `stageKind`.

### 4.3 Active panel selection

Choose the canvas panel in this order:

1. The remembered active panel, if it still exists in this stage generation.
2. The selected element's nearest owning `IdtxWidgetPanel` ancestor.
3. `defaultPrim`, when it is a panel.
4. The sole available panel, or an explicit panel selector if several exist.

A tagged stage without a usable panel still opens Widget Editor with an empty
state and **Create Widget Panel** recovery action. Selecting a helper prim leaves
the current canvas panel visible and inspects that prim.

### 4.4 New widget stage

Add **File → New Widget Stage** using the existing project-aware save flow:

- Create `/Panel` as `IdtxWidgetPanel` and author `defaultPrim = "Panel"`.
- Author the root tag above and the new layout version.
- Seed Rectangle, sensible logical/world dimensions, raster resolution, and
  native child content. Offer an Image/Label/Button insertion toolbar.
- Save and reopen through the standard loader so activation uses the same path
  as every other widget document.

## 5. Authored layout and panel-shape contract

### 5.1 Separate the coordinate systems

| Space | Meaning |
| --- | --- |
| USD/world units | Existing `idtx:widget:size` and panel transforms; determine world-space presentation. |
| Logical layout pixels | Panel-local authoring coordinates, top-left origin, +X right, +Y down. Explicit pixel sizes and offsets use these units. |
| Normalized coordinates | Anchors, pivots, and percentage values; independent of texture density. |
| Raster texels | Actual offscreen texture dimensions, subject to resolution, quality scaling, and device limits. |
| Editor screen points | Egui input/display coordinates, mapped through canvas pan/zoom and display DPI. |

Add a `designSize` for the reference authoring/world layout. For world panels,
logical layout resolves at that size. For responsive HUD panels, it resolves at
the actual logical on-screen panel size. Editor device presets supply that same
HUD layout context. Raster quality changes only the logical-to-texel mapping.

### 5.2 Proposed attributes

Add these to the existing codeless widget schemas and Rust vocabulary. Use typed
attributes so USD composition, overrides, inspection, and packaging work normally.

| Owner | Proposed property | Type and rule |
| --- | --- | --- |
| Panel | `idtx:widget:layoutVersion` | `uniform int`; fallback `1` preserves legacy behavior; new editor documents author `2`. Independent of the root tag's `schemaVersion`. |
| Panel | `idtx:widget:designSize` | `float2`; positive logical width/height; templates author explicitly. |
| Panel | `idtx:widget:shape` | `uniform token`: `legacy`, `rectangle`, `circle`; fallback `legacy` resolves the existing polygon arrays or the legacy rectangle. |
| Element | `idtx:widget:layout:placement` | `uniform token`: `absolute` or `flow`. New free-form elements explicitly use `absolute`. |
| Element | `idtx:widget:layout:anchor` | `float2` in `[0,1]`; anchor in the immediate containing content box; nine standard presets. |
| Element | `idtx:widget:layout:pivot` | `float2` in `[0,1]`; point of the element box attached to the anchor. |
| Element | `idtx:widget:layout:offset` | `float2`; signed displacement from the anchor/allocated rectangular flow placement. `(0,0)` is a valid value. Inactive for radial slots. |
| Element | `idtx:widget:layout:offsetUnit` | `uniform token`: `pixels` or `percent`; percent values are stored as fractions and displayed as percentages. |
| Element | `idtx:widget:layout:size` | `float2`; requested width/height interpreted per axis. Radial slots instead derive sector and caption geometry from their container. |
| Element | `idtx:widget:layout:widthUnit`, `heightUnit` | `uniform token`: `pixels`, `percent`, or `auto`. Percentage is relative to the parent content box for absolute placement, or the allocated slot/cell for rectangular flow; auto uses defined intrinsic/measurement rules. |
| Element | `idtx:widget:layout:aspectRatio` | `float`; positive width/height ratio, `0` means unconstrained. Used when one dimension is derived. Image intrinsic ratio can supply the auto-axis ratio. |
| Text-capable types | `idtx:widget:text:wrap` | `uniform token`: `none`, `word`, `character`. |
| Text-capable types | `idtx:widget:text:overflow` | `uniform token`: `clip`, `ellipsis`. |
| Text-capable types | `idtx:widget:text:horizontalAlignment`, `verticalAlignment` | `uniform token`: start/center/end and top/center/bottom. |
| Text-capable types | `idtx:widget:text:autoFit` | `bool`; explicit shrink-to-fit opt-in for version 2, default false. |

Arrangement-specific additions are part of the same version-2 contract:

| Owner | Proposed property/change | Type and rule |
| --- | --- | --- |
| Container | Extend `idtx:widget:layout` | Existing `uniform token`; add `free` and `table` to `row`, `column`, `grid`, `stack`, `radial`. Keep the schema fallback `column`; new Free templates author `free` explicitly. |
| Element | `idtx:widget:layout:flowOrder` | `int`; orders participating siblings and button cycling, with namespace-path ties. When unauthored, use the existing `idtx:widget:order` for compatibility; new elements and migration author it explicitly. Paint order then remains independently editable. |
| Grid container | `idtx:widget:grid:columns` | `uniform int`; `0` means automatic near-square packing, positive values fix the column count. Rows are derived from participating children. |
| Table container | `idtx:widget:table:columns` | `uniform int`; positive shared column count, template default `2`. |
| Table container | `idtx:widget:table:columnWeights` | `float[]`; empty means equal columns, otherwise one positive finite relative weight per column. Divide the width remaining after padding and inter-column gaps by these weights. |
| Element in Table | `idtx:widget:table:cell` | `int2` stored as `(column, row)`, zero-based; `(-1,-1)` means automatic row-major placement. Show separately labelled Row and Column controls in the inspector. |
| Element in Table | `idtx:widget:table:span` | `int2` stored as `(columnSpan, rowSpan)`, default `(1,1)`; both positive. Spans must fit the column count and occupy non-overlapping cells. |

Reuse the existing `opacity`, `order`, `visibility`, font/content properties,
container `gap/padding/alignment`, radial settings, and image `fit`. Expose font
properties consistently on Labels, Buttons, and Fields in the schema. Arrangement
properties remain authored when inactive so composition and undo retain them.

For version 2, define `fontSize`, container gap, and padding in logical pixels.
The current schema describes font size as points while the renderer interprets
it as raster pixels; document and test the versioned correction.

For absolute placement, with containing box origin `P`, size `S`, resolved
element size `E`, anchor `A`, pivot `V`, and resolved offset `O`:

```text
element_top_left = P + A * S + O - V * E
element_bottom_right = element_top_left + E
```

Anchor presets set compatible anchor/pivot values. **Center** uses `(0.5,0.5)`
for both; zero offset centers the element exactly. Unit changes and supported
rectangular placement-mode conversions compute new authored values from the
existing resolved box to preserve placement. Changing a container arrangement is
a separate reflow/conversion operation under section 5.6; rectangular boxes cannot
represent radial sector geometry losslessly.

Auto measurement must have a defined dependency order: resolve available width,
measure wrapped text or image aspect, then resolve height. Detect cyclic cases
such as percentage children determining an auto-sized parent. The first release
can limit auto container sizing to acyclic, bounded cases.

### 5.3 Rectangle and circle semantics

- **Surface Shape** belongs to the panel. **Arrangement** belongs to each child
  container. Rectangle + Radial and Circle + Row/Column/Grid/Table/Free are valid
  combinations; several differently arranged containers can share one panel.
  Changing either control does not implicitly change the other. The existing
  transparent radial-menu example uses a rectangular panel surface, not a Circle.
- **Rectangle:** the entire logical panel box is the surface.
- **Circle:** use an analytic disk and a square logical canvas. The panel inspector
  exposes a diameter and links its two logical axes; world presentation exposes a
  stage-unit diameter. HUD placement resolves a centered square using the smaller
  requested dimension. This makes Circle a circle across presentation contexts.
- Raster dimensions can differ from logical dimensions; surface tests always map
  texels back into logical coordinates.
- A new Circle uses the same analytic surface for background coverage, child
  clipping, editor bounds, picking, and movement containment.
- Existing polygon arrays remain the `legacy` surface representation. The
  inspector displays an existing custom polygon as such and offers explicit
  Rectangle/Circle conversion. Preserve its authored arrays for undo/round-trip.
- Shape/diameter changes are grouped edits. Revalidate existing children and show
  overflow plus a **Fit Elements to Panel** command; that command is a separate,
  atomic layout operation rather than an implicit lossy resize.

### 5.4 Compatibility and migration

Keep the current version-1 layout as an explicit compatibility resolver. Merely
opening, selecting, or saving an old widget should not migrate its layout.

Provide **Upgrade Layout** and use the same migration transaction before the
first version-2 spatial edit of a legacy panel:

1. Capture all relevant authored opinions and the old resolved layout at its
   reference resolution.
2. Materialize explicit boxes/offsets from the actual old geometry. This handles
   `(0,0)` being a sentinel and legacy widths/heights being inferred from siblings.
3. Preserve radial subtrees using their existing settings and allocated bounds;
   retain polygon surfaces and namespace identities. Materialize the existing
   sibling sequence as `layout:flowOrder`, including disabled button slots.
4. Seed `designSize`, explicit placement/units, and `layoutVersion = 2` together.
   Rectangular legacy leaves can use absolute boxes to preserve their appearance
   even where previously authored gap/padding were ignored.
5. Compare resolved geometry before committing. Report unsupported conversions
   without a partially migrated document. Undo restores the original opinions.

Migrate the owning panel before changing a legacy container's arrangement or
adding/removing its first/last radial subtree. Today these changes can switch the
layout algorithm for the entire panel. Version 2 must resolve all arrangements
through one hierarchy so unrelated siblings do not jump when radial content
appears or disappears. Migration and the requested edit form one undoable action.

New attributes, source schema, generated runtime resources, and documentation
must land together. Regenerate through the documented
[`gen-schema` workflow](crates/idtx-usd-extension/README.md), and verify schema
fallbacks and inheritance with the registered plugin.

### 5.5 Container arrangements: USD audit and editor representation

#### What the current USD/runtime actually supports

| Finding | Evidence and implication |
| --- | --- |
| Radial is a mode of an ordinary Container. | The [container schema](crates/idtx-usd-extension/schema/schema.usda) and [`WidgetLayout`](crates/idtx-usd-extension/src/lib.rs) define `row`, `column`, `grid`, `stack`, and `radial` on `IdtxWidgetContainer`. There is no distinct radial prim type, and no schema/type barrier to changing the token. |
| A radial slot is an authored Button whose sector is derived by its parent. | [`radial_menu.usda`](examples/widgets/radial_menu.usda) authors `/Menu/Items` with `layout = "radial"` and immediate Button children. [`five_actions.usda`](examples/widgets/five_actions.usda) adds slots and overrides enabled state through a reference. Captions, colors, enabled state, and callback paths belong to the same Button prims. |
| Only immediate Buttons participate in radial geometry. | [`compute_widget_layout`](crates/idtx-scene/src/widget_layout.rs) skips every non-Button child in its radial branch, including nested containers and their subtrees. It sorts slots by `order`, then path. Disabled Buttons keep their sectors; hidden/effectively zero-opacity elements have already been excluded by [hydration](crates/idtx-scene/src/hydrator/widget.rs). |
| Radial changes shape and interaction, not just position. | The parent generates `AnnularSector` shapes and label boxes. Per-Button `position` is ignored by this branch. [`radial_stick_target`](crates/idtx-scene/src/widget_layout.rs) requires a radial container and uses sector angles, gaps, dead zone, and hysteresis. Ordinary rectangular Buttons have no equivalent per-element sector attributes. |
| Existing rectangular arrangements have partial runtime support. | The hierarchical resolver implements Row, Column, Stack, and an automatically packed Grid (`ceil(sqrt(child_count))` columns), but is entered only when the panel contains a radial container. Rectangle-only panels use legacy flattened slot heuristics. Authored gap/padding/alignment are not consumed by this resolver. |
| Free and Table are new capabilities. | Neither token, table tracks/cells, nor a configurable grid column count exists in the current schema/runtime. Unknown layout tokens currently parse to the default Column during hydration; authoring `table` alone would not implement a table. |

The editor should therefore expose **Surface Shape**, **Arrangement**, and
**Placement in Parent** as separate concepts. A container can, for example, have
absolute placement in its panel, arrange its children as a Row, and contain a
Radial child container occupying one row slot. Its own arrangement does not decide
how its parent places it. Use the concrete arrangement names rather than an
ambiguous **Normal / Radial** toggle.

#### Version-2 arrangement contract

All choices below use `IdtxWidgetContainer` and the existing `idtx:widget:layout`
property. Menu presets select the mode and initialize its settings; they do not
introduce separate Row, Table, or Radial prim types.
Enable the new Free/Table choices together with schema, hydration, resolver, and
packaged-runtime support. An unrecognized arrangement must display an unsupported
value diagnostic in the editor rather than masquerading as Column.

| Arrangement | Participating children and allocation | Inspector controls |
| --- | --- | --- |
| Free (`free`, new) | All widget element types; children use explicit absolute boxes, without managed slots. | Container box, child anchors/offsets/sizes; padding defines the available content box. |
| Row (`row`) | All element types, including containers; flow children occupy a horizontal sequence. | Gap, padding, cross-axis alignment, flow order. |
| Column (`column`) | All element types, including containers; flow children occupy a vertical sequence. | Gap, padding, cross-axis alignment, flow order. |
| Grid (`grid`) | All element types; flow children pack row-major into uniform cells. Automatic or explicit column count; derived row count. | Columns (Auto or positive count), gap, padding, cell alignment, flow order. |
| Table (`table`, new) | All element types; children occupy shared row/column tracks, with explicit or automatic cell assignment and optional spans. | Column count/weights, gap, padding, cell alignment; Row/Column and spans on each participating child. |
| Stack (`stack`) | All element types; flow children share the content area and overlap, with separate paint order. | Padding, alignment, paint order; linear gap has no effect. |
| Radial (`radial`) | Immediate leaf Buttons only; the container derives annular sectors and caption boxes. Disabled Buttons retain their slots. | Center, inner/outer radius, start/sweep/gap angles, slot sequence, dead zone, and hysteresis; selected Button caption/color/enabled controls. |

Row/Column/Grid/Table/Stack also allow explicitly absolute children as overlays;
these do not consume slots/cells. New insertions default to flow. Free children
are absolute; Radial children are flow-controlled Buttons. Slot participation and
the corresponding inspector controls must come from one shared capability model.

For Row/Column, allocate equal shares of the content area's main axis after gaps;
Grid divides the remaining area into uniform tracks; Stack uses the whole content
box. Resolve a flow child's requested size/alignment within its allocation, then
apply its rectangular managed offset. Intrinsic/percentage sizes do not silently
change the sibling allocation policy. More advanced weighted linear layouts can
extend this contract later.

Display the mode as a scene-tree badge, for example **Items · Container (Radial)**.
Selection remains the actual Container or Button prim. Show container bounds,
sector guides, grid lines, and table cells as editor overlays; computed slots and
cells are not additional USD prims. Conditional inspector sections expose only
parameters that affect the selected arrangement.

**Radial settings and editing:**

- Preserve the current angular convention: zero is up, positive is clockwise,
  start is inclusive, and end is exclusive. Radii use the smaller allocated
  dimension; version 2 applies them to the container's padded content box. Center
  is normalized in that box. The legacy resolver retains its existing bounds.
- Validate finite values, normalized center components in `[0,1]`,
  `0 <= innerRadius < outerRadius <= 0.5`, positive sweep up to 360 degrees, and a
  nonnegative gap smaller than the resulting slot angle when there are visible
  slots. Preserve the current constant-width, parallel-sided gap geometry
  described in [`WIDGETS.md`](WIDGETS.md), including the outer-radius reference for
  a zero inner radius. `deadZone` is in `[0,1)` and `hysteresisAngle` in `[0,45]`
  degrees.
- Radial uses `radial:gapAngle`; generic linear gap and cross-axis alignment are
  inactive. Container padding and placement remain applicable in version 2.
- Caption text is the Button's `label`; a Label child or Image child is not a
  supported radial slot. Keep imported unsupported children inspectable in the
  tree with a layout diagnostic, rather than silently deleting or retyping them.
- Edit slot sequence through `layout:flowOrder` and move the whole menu through
  its container. Individual sector translation/size is read-only in the initial
  editor; section 7.3 defines the movement affordance.

**Table authoring:**

1. Keep cell elements as immediate children of the Table container. A cell needing
   a Label plus a Field can contain another Container. Independent Row containers
   alone do not provide shared column widths across rows.
2. Reserve explicit cells/spans first; diagnose collisions and out-of-range
   columns. Place automatic children, in flow order, in the first available
   row-major region that fits their span. Validate assignments across composed
   children, including hidden children, so revealing a cell cannot create an
   unreported collision. New insertions author the chosen cell explicitly.
3. Derive row count from occupied cells and spans, retaining explicit empty cells
   between assignments. Empty tables still have a selectable container box.
   Allocate shared weighted columns and, initially, equal-height rows after
   padding/gaps. A spanned allocation includes its intervening track gaps.
4. Resolve each child's dimensions, wrapping, alignment, and clipping within its
   allocated cell. Validate column-count, weight, cell, and span edits together;
   reject a conflicting edit without partially updating the table.
5. Ship this basic table arrangement with version 2. Fixed/intrinsic track sizing,
   header semantics, repeated data-bound rows, sorting, and virtualization are
   separate extensions. Typed Fields in authored cells use their existing value
   and compute-binding contract.

### 5.6 Changing arrangements: feasibility and conversion policy

**Conclusion:** USD allows changing the arrangement token on the same Container.
A universal, appearance-preserving **Normal ↔ Radial** switch is not possible with
the current representation: non-Button children disappear under Radial, and a
sector cannot be preserved as a free rectangular element. Offer validated
arrangement changes with explicit reflow semantics, rather than treating them as
panel-shape changes or raw token edits.

| Change | Editor policy |
| --- | --- |
| Empty container → any supported arrangement | Allowed; initialize destination settings and retain the container's placement in its parent. |
| Row / Column / Grid / Stack ↔ one another | Allowed reflow of participating children; retain absolute overlays and child identities. Re-resolve offsets/alignment and show the resulting layout. |
| Rectangular managed arrangement → Free | Bake each child's current final rectangle into absolute placement. Preserve visible geometry and nested container allocations at the current layout context; future responsive reflow follows the newly authored absolute/anchor contract. |
| Free → Row / Column / Grid / Stack / Table | Explicitly enroll its children in flow and reflow them; initialize managed offsets and, for Table, cell assignments. The preview shows the intended geometry change. |
| Non-radial → Radial, all widget children are immediate leaf Buttons | Allowed as **Convert to Radial**. Enroll all Buttons as slots, preserve their paths/content/enabled state and sequence, and derive sectors. Rectangular box/offset settings become inactive; sector shape and stick-navigation semantics take effect. |
| Non-radial → Radial with Labels, Fields, Images, Sliders, Toggles, nested containers, or widget descendants outside immediate Button slots | Unavailable; identify the incompatible paths, including hidden and referenced children. Author a separate button-only Radial container when this content needs to coexist. Conversion does not drop, reparent, or change the types of existing elements. |
| Radial → Row / Column / Grid / Stack / Table | Allowed explicit reflow to rectangular Buttons. Retain radial settings as inactive opinions; use or initialize destination box/cell settings. Directional radial navigation no longer applies. |
| Radial → Free | Offer **Convert Sectors to Rectangular Buttons** using the resolved label boxes as initial absolute boxes. This preserves Button identity/content but deliberately replaces sector shapes. A lossless **Free Position** conversion of a sector is unavailable. |
| Other arrangement → Table | Assign/validate flow children's cells and spans before commit; seed missing assignments row-major and default shared column weights. Retain absolute overlays, except when explicitly enrolling Free children as above. Radial still requires sector-to-rectangle conversion. |
| Table → another rectangular arrangement | Reflow, or bake final cell rectangles for Free. Keep cell/span/track opinions inactive so a later return to Table can validate and reuse them. Table → Radial follows the same Button-only rule as every other source. |

Implement these through a shared `ChangeWidgetArrangement` command:

1. Evaluate the full composed subtree and current resolved layout at the selected
   edit target. Validate the destination's child rules, finite geometry, cell
   assignments, and owning-panel/ancestor containment. Eligibility must include
   elements omitted from visible paint data. Detect stale menu/inspector intents.
2. Build a before/after layout preview. Preserve namespace paths, concrete types,
   values, compute connections, and action bindings. Mode-specific properties
   remain authored but inactive; no implicit panel-surface change or namespace
   rewrite accompanies an arrangement change.
3. For legacy panels, perform the geometry-preserving version-2 migration first
   as part of the same transaction. Seed explicit flow order and destination
   properties, then atomically author the mode and required child changes. A
   failure restores all captured opinions, including previously absent values.
4. Rebuild layout, pixels, picking, and movement capabilities together; clear
   stale focus/highlight/gesture state. Undo restores the exact previous layout
   opinions. Selecting the old mode later recomputes with current children and
   settings and is not a substitute for undo.

Use the same validation for the dedicated inspector, generic edits to the layout
token, and structural insert/remove commands. Adding/removing a radial Button
changes every sector's angular allocation; revalidate gap/sweep and containment
against the new slot count before commit.

The runtime's [`widget_navigation`](crates/idtx-studio-session/src/scene_backend_impl.rs)
uses radial stick selection for `step = 0`; button cycling with `step = ±1` is
separate. In version 2, cycling must consume the resolved flow/slot sequence rather
than the paint-sorted list. Keep path-addressed click handlers and authored behavior
configuration, and expose the navigation change in the conversion preview. Merely
retaining a Button's path cannot preserve a behavior's radial stick interaction
after a conversion to Row, Table, or Free. Navigation/activation regression
coverage is a required part of the conversion workstream.

## 6. Orthographic canvas and picking

### 6.1 Canvas interaction

Implement a pure egui canvas module with:

- A checkerboard background, panel boundary, optional grid, selection outlines,
  anchor/pivot markers, and a translation gizmo when the selection's arrangement
  permits movement.
- Wheel/pinch zoom around the pointer, middle-button or Space-drag pan,
  **Frame Panel**, **Frame Selection**, and **100%** actions.
- A fixed front-facing projection. Route canvas gestures and keyboard shortcuts
  to this canvas instead of the viewport orbit/projection actions.
- A device-size selector and a distinction between changing preview dimensions
  and editing authored `designSize` or raster resolution.

Maintain one invertible mapping between logical panel coordinates and the drawn
canvas rectangle. Include panel placement within the canvas, letterboxing, pan,
zoom, and DPI exactly once. Rendering overlays and unprojecting the pointer use
that same mapping.

For ray-based reuse, unproject the pointer to a near-plane point and cast parallel
to the panel normal. The existing [`ray_from_viewport`](crates/idtx-studio-shell/src/render.rs)
call site already uses per-pixel near-plane origins for orthographic views; a
single camera-eye origin would select the same location for every click.

### 6.2 Derived extents and reuse of broad/detail picking

**Recommendation: author element layout size; derive element picking extents.**

Add a panel-scoped `WidgetPickIndex` in `idtx-scene`, populated from the resolved
layout. Each leaf records the existing element identity, its conservative AABB,
shape reference, clip chain, and paint rank. A small thickness around the panel
plane makes broad-phase slab tests robust; detail hits still use the exact plane.

Reuse the existing `ray_aabb` math and factor the runtime `ray_panel` /
`point_panel_uv` helpers into the shared scene geometry layer. Start with a cached
linear candidate list; retain a seam for the existing BVH machinery if measured
panel sizes justify it. The widget query has an explicit owning-panel scope.

This resolves the proposed `extent` approach without making responsive element
bounds stale in USD:

- An element's box depends on parent layout, anchors, fonts, and the current HUD
  size. Its computed extent is a projection of those inputs.
- The current elements are Imageable, and the native projection checks Boundable
  and plugin-computed extents. A new property named `extent` alone would not
  register a pickable entity or create an appropriate detail test.
- Converting every element to Boundable/Xformable would introduce different
  schema semantics and still require layout-aware extent recomputation.
- Preserve the Imageable hierarchy. Use derived editor picking records associated
  with the existing USD prims; expose computed bounds read-only in the inspector.
- Derive panel bounds from its resolved size/surface as well. This also supports
  reliable panel framing and optional panel-level selection in 3D View.

If a later interoperability requirement calls for an authored extent cache,
specify its coordinate space, layout-context/version key, and invalidation rules.
Treat it as generated data with recomputation on a cache miss, never the source
of truth for editor selection.

### 6.3 Picking algorithm

1. Check editor chrome/gizmo handles first. A consumed pointer gesture cannot
   also emit a scene selection or runtime widget click.
2. Map the click through the current canvas mapping to a panel-local ray/point.
   Reject clicks outside the panel's displayed surface/clip.
3. Broad phase: test the active panel's derived element AABBs using the shared
   slab test. Handle zero ray components, boundary hits, and degenerate boxes.
4. Detail phase: intersect the panel plane and test the final rectangle/sector
   shape, ancestor clips, and panel Rectangle/Circle/polygon surface.
5. Resolve overlapping leaf elements in reverse actual paint order. Match the
   renderer's deterministic order/path tie-breaks.
6. Fall back to the deepest eligible container in an uncovered area, then the
   panel itself. A click outside the panel clears selection.
7. Return `WidgetEditorHit { panel, element, uv, logical_position,
   layout_revision }`; `element` is absent for a panel/background hit.
8. Resolve the exact composed element path, set session selection, reveal/scroll
   the corresponding tree row, and refresh its inspector.

All elements are coplanar. The existing generic picker's nearest-distance
early-exit (`t_aabb >= best.t`) cannot decide element stacking: evaluate relevant
equal-depth candidates or iterate them by paint rank. Avoid introducing artificial
Z offsets to encode widget order.

### 6.4 Editor selection versus runtime interaction

- Disabled buttons remain selectable and editable. Movement eligibility depends
  on their arrangement, exactly as for enabled buttons.
- Selection does not depend on registered behavior handlers, click capability,
  or the control's runtime enabled state.
- Text and images select by their layout/hit shape, including whitespace inside
  their box. Per-pixel image alpha picking can be a later explicit option.
- Hidden/fully transparent elements remain accessible in the tree. An editor
  **Show Hidden Elements** mode can expose their dashed boxes for recovery.
- Empty transparent panel space can select its authoring surface. Runtime
  occlusion retains its established background/element-coverage policy.
- Existing world interaction continues to return the panel plus element-relative
  identity through the behavior interface. Reuse layout geometry without changing
  its wire identity or dispatching callbacks from editor clicks.

Tag input with the layout revision used to draw the frame. If a panel, prim, or
stage generation disappears before dispatch, discard the stale intent. Rebuild
the pick index when layout changes so rendered and selectable geometry advance
together.

## 7. Translation gizmo, constraints, and authoring transactions

### 7.1 Gizmo behavior

Draw screen-size-stable X and Y handles plus a center free-move handle at the
selected movable element's pivot. Determine movement capability from its parent's
arrangement, as specified in section 7.3. Direct dragging may use the same move
operation. The panel root exposes its dimensions; a radial Button exposes slot
editing and an **Edit Container** link in place of element-translation handles.

On pointer-down, capture the element's authored layout opinions, target layer,
resolved box, panel identity, layout context, and screen-to-layout mapping.
Compute each update from that starting state plus the total pointer displacement.
This avoids accumulating rounding error or using a just-rehydrated value as a new
drag origin.

Author `layout:offset` in the element's selected units. A center-anchored element
keeps that anchor and pivot during movement. The 2D gesture writes no element
`xformOp` values.

### 7.2 Constrain the complete element footprint

For a rectangle panel of logical size `(W,H)` and an axis-aligned element box
with top-left `(x,y)` and size `(w,h)`:

```text
0 <= x <= W - w
0 <= y <= H - h
```

Apply this to the resolved box after anchor/pivot conversion. Clamping only the
position or pivot to `[0,1]` lets most of an element leave the panel.

For a circle with center `C` and radius `R`, a rectangular element is valid iff
all four corners satisfy:

```text
length(corner - C) <= R
```

The legal translations form an intersection of four disks. Implement a pure
closest-feasible-translation helper, including a constrained-axis variant.
Projection must allow sliding along the circular boundary; simply clamping the
center to the disk, or stopping every movement segment at its first collision,
produces incorrect or sticky dragging.

Additional rules:

- Account for ancestor content clips as well as the owning panel boundary.
- Moving a container moves its descendant layout with it; use its resolved
  footprint and relevant descendant overflow when determining legal movement.
- A radial container's rectangular allocation frame is a layout/picking aid,
  not a filled surface. Test its generated sector footprints before panel
  clipping, rather than requiring the frame's unused corners to fit in a Circle.
  A centered radial menu can therefore fill a circular panel of the same diameter.
- Use shape-aware extrema for existing nonrectangular elements such as sectors.
  Their conservative broad-phase AABB is not necessarily their containment shape.
- If an element cannot fit at any position, keep its dimensions, show the
  constraint result, and allow size editing. Never emit a negative clamp range
  or non-finite authored value.
- Numeric position edits, keyboard nudges, unit conversion, alignment commands,
  and dragging share the same validation/constraint implementation.
- Re-evaluate constraints when preview dimensions change. An invalid imported
  position can be recovered by projecting it into the legal region when it fits.

### 7.3 Managed layouts

Use the shared arrangement capabilities for gizmos, numeric controls, keyboard
nudges, and insertion defaults:

| Placement context | Movement and editing contract |
| --- | --- |
| Direct panel child or child of Free | Absolute XY movement with anchors, size, and full-footprint containment. A Container moves its descendants coherently. |
| Flow child of Row / Column / Grid / Stack | The offset translates the final rectangular box after allocation, while sibling slots remain deterministic. The inspector labels it **Offset from Arrangement**. Sequence edits use flow order, not paint order. |
| Flow child of Table | Row/Column and span controls change cell allocation through occupancy validation. An XY offset moves content within its allocated cell/clip without moving the cell or changing shared tracks. |
| Absolute overlay in a rectangular managed container | Ordinary absolute XY movement in the parent's content box; the child does not participate in slot/cell allocation. |
| Button slot of Radial | Container-controlled sector/caption geometry; no independent XY, width/height, anchor, or pivot editing. Edit caption/color/enabled state and slot sequence, or select the parent to move/resize/rotate the arrangement via its box and radial settings. |

Apply a rectangular managed offset consistently to shape, text placement, and the
element's content box, while retaining its ancestor/slot clip. **Free Position**
on a rectangular flow child bakes its resolved rectangle and removes it from flow;
show any resulting sibling reflow in the preview. Radial slots have no equivalent
lossless detach operation; use the explicit conversion in section 5.6.

Moving a radial container translates the entire menu, including sector centers,
caption boxes, clipping, picking, and navigation geometry. Keep regression tests
for this operation. Independent sector offsets would require a new shape and
directional-navigation contract: the current stick selector uses angular spans
and does not account for arbitrary per-sector XY translation. Treat that as an
advanced follow-up rather than exposing a gizmo that authors ineffective values.

### 7.4 Snapping and keyboard input

- Grid spacing is an editor preference in logical pixels; start at 8 pixels with
  visible grid and snap toggles. Changing zoom affects grid display density only.
- Snap the dragged pivot/selected reference point against a fixed panel-local
  grid origin. Show the reference point and snap indicator.
- Apply axis constraints, resolve the grid target, then enforce full-footprint
  containment. Prefer a legal grid location when available; containment wins at
  boundaries where a grid target is infeasible.
- Provide a temporary snap-bypass modifier and arrow-key nudging: one logical
  pixel normally, one grid step with Shift. Consume these only while the canvas
  owns keyboard focus.
- Gizmo capture continues outside the canvas until release; text-field editing,
  panning, and 3D shortcuts do not consume the same gesture.

### 7.5 Undo, cancellation, and edit targets

Use one property-gesture transaction for inspector layout edits and canvas moves:

```text
Begin -> capture exact authored opinions
Update -> validate, constrain, author changed values, refresh preview
Commit -> one before/after journal entry
Cancel -> restore captured opinions, no completed history entry
```

Extend the journal with grouped property changes. Capture the exact edit-target
spec, including absence, declared type, metadata, value blocks, and time samples.
An undo must restore an absent opinion instead of writing the composed fallback.

The current inspector helper captures root properties even though the sink can
carry a session/variant target. Make widget transactions explicitly target-aware:
record the target layer and mapped prim-spec path, and replay to that target.
If a target cannot support reversible authoring, its editing controls must report
that state instead of routing through a root-only capture.

- Coalesce movement updates to one authoring update per frame; commit once.
- No-motion drags create no history entry.
- Escape cancels; focus loss or document/center-tab change cancels the unfinished
  gesture before changing context.
- Deletion, reparenting, edit-target changes, or a conflicting update to the same
  properties invalidates the gesture. Preserve unrelated newer opinions when
  restoring/replaying captures, following the existing delta-replay approach.
- A failure in a multi-property edit rolls back the whole edit. Anchor presets,
  unit conversions, circle-diameter changes, arrangement conversions, table
  track/cell edits, migration, and Fit Elements must be atomic and have useful
  undo labels.

## 8. Inspectors and widget-only scene-tree authoring

### 8.1 Dedicated inspector contributions

Register widget contributions before the fallback inspector and claim the
properties they actually implement. Include authored/default indicators and the
existing property opinion controls.

| Selection | Authoring controls |
| --- | --- |
| Panel | **Surface Shape: Rectangle / Circle**; logical size/diameter; world size with stage-unit labels; requested and effective raster resolution; density/quality information; background color and opacity; panel opacity; visibility; world/HUD space; billboard; double-sided; HUD placement. |
| Any element | **Placement in Parent**, with arrangement-aware anchor/pivot/offset/size controls; units, aspect lock, separate flow order and paint order, opacity, visibility, owning-panel breadcrumb; computed bounds read-only. Radial slot geometry is read-only. |
| Container | **Arrange Children → Arrangement: Free / Row / Column / Grid / Table / Stack / Radial**; conversion eligibility/preview; mode-specific gap/padding/alignment, Grid column count, Table shared tracks, or Radial center/radii/angles/navigation controls from section 5.5. Its own Placement in Parent is independent. |
| Child in Table | Row/Column, row/column spans, automatic/explicit assignment, allocated cell box, and collision/track validation; other controls depend on the concrete element type. |
| Label | Multiline text, font family/asset, font size, color, wrap, overflow, and text alignment. |
| Button | Caption, enabled state, fill color, font and text controls. |
| Image | Resolver-backed image asset, fit mode, intrinsic dimensions/aspect, and missing-asset status. |
| Slider | Typed value/min/max; preview reflects the authored range. |
| Toggle | Boolean value. |
| FieldDouble / FieldFloat | Correctly typed value, precision, font/text styling, and existing compute-connection information. |
| FieldInt / FieldString | Integer/string value, appropriate text styling, and compute-connection information. |

Keep a FieldDouble value as `f64` through authoring and formatting. The rasterizer's
display string or scalar `f32` slot is not a suitable inspector source value.

Add the required typed support end-to-end:

1. `AttributeValue::{Float2, Int2, FloatArray, Float2Array, IntArray}` where needed
   by widget controls, and `PropertyValue::{Float2, FloatArray, Float2Array}` in the
   safe USD layer. Table column weights need `float[]`; cells/spans need `int2`.
2. Both `InspectorEditSink` conversions, gesture capture/replay, and value-size
   accounting in the journal.
3. Native `float2`/`float[]`/`float2[]` writers and typed reads with finite-value
   validation; retain correct USD roles rather than substituting color/vector/point
   types. Preserve authoredness for the flow-order compatibility fallback.
4. Schema-aware property creation that preserves uniform/custom declarations.
5. Shared layout validation for edits emitted through generic property controls
   as well as dedicated widget controls.

Use the existing editability policy, with an inspector action to create a local
opinion on referenced elements where necessary. Surface renderer capability
information for `contentSource` and other unsupported advanced properties, as
specified in chapter 9.

### 8.2 Scene-tree policy

Replace widget-stage decisions scattered across menus with a UI-neutral
`SceneAuthoringPolicy::Widget` passed through `ActiveTabData`. Apply it whenever
the active stage is tagged widget, regardless of which center tab is visible.

**Add Widget Element** opens a type-selection submenu, not an undifferentiated
creation action. For a panel or a supported non-radial container, offer:

```text
Add Widget Element ▶
  Field ▶
    Double
    Float
    Integer
    String
  Image
  Button
  Label
  Slider
  Toggle
  Container ▶
    Free
    Row
    Column
    Grid
    Table
    Stack
    Radial
Remove Widget Element             [only for a removable element]
```

Each leaf choice dispatches a concrete template ID. Field choices create
`IdtxWidgetFieldDouble`, `IdtxWidgetFieldFloat`, `IdtxWidgetFieldInt`, or
`IdtxWidgetFieldString`, respectively. Container choices all create
`IdtxWidgetContainer` with the selected arrangement and its defaults. Keep these
choices inside Add; they are not additional top-level authoring entries.

| Context-menu target | Allowed entries |
| --- | --- |
| Owning panel, including the default/root panel | Add Widget Element with the full submenu above. Panels are not removable widget elements; edit dimensions and surface shape in their inspector. |
| Free / Row / Column / Grid / Table / Stack container | Add Widget Element with all concrete choices, and Remove Widget Element, subject to editability. |
| Radial container | Add Widget Element → **Button** only, and Remove Widget Element. Other kinds do not satisfy the current slot contract. |
| Leaf widget element, including a radial Button | Remove Widget Element only. |
| Helper/non-widget prim or empty space without a valid parent | No widget add/remove command for an invalid target; panel recovery uses the canvas action from section 4.3. |

Use explicit parent/child rules. The tree's current blanket gprim-child policy
is not appropriate for a Panel → Imageable widget-element relationship.
Abstract `IdtxWidgetElement` and `IdtxWidgetField` types are never creation choices.
Build the submenu from the same `allowed_templates(parent)` capability query used
by dispatch. Menu availability uses the composed parent's arrangement and current
editability; stale intents must recheck them before creating anything.

The widget context menu contains no generic shape/camera/light creation,
reference/payload insertion, behavior attachment, rename, or opinion-management
entries. Naming and opinion management can live in the inspector; dedicated
behavior/compute tools own their helper prims.

Implement widget-specific creation/removal commands with validation at dispatch,
not just menu filtering. Reuse that validation for Delete keys, insertion
toolbars, duplication/paste, and drag/drop paths that target the widget document.
It must prevent a stale generic tree intent from creating arbitrary 3D children.

Creation templates author type-appropriate content, choose a unique name, select
the new prim, seed explicit flow order, and create one undo entry. Placement
depends on the parent:

- Panel/Free: seed an explicit absolute box that fits inside the owning surface.
- Row/Column/Grid/Stack: append a flow child with explicit sequence and suitable
  dimensions; resolve the updated arrangement before commit.
- Table: assign the next available cell, initialize a `(1,1)` span, and validate
  against composed occupancy and shared tracks.
- Radial: append a Button slot, seed caption/color/enabled state and sequence, and
  recompute sector geometry. Derive its size/position from the arrangement.
- Container presets also seed a usable container box and arrangement settings;
  their child lists start empty. The inspector/canvas can select empty containers.

Validate the resulting layout and containment, including changed sibling slots,
through the same resolver used for arrangement conversion. Removal deletes a
locally owned definition or authors the proper local deactivation for composed
content; deleting only an `over` must not reveal the supposedly removed element
again. Removing a selected subtree selects the surviving parent/panel and clears
all stale gesture/pick data.

The existing collaboration create protocol only accepts a fixed set of built-in
3D prim types. Advertise actual widget-operation support through capabilities;
until the protocol supports typed widget creation/removal, those operations are
unavailable in live collaboration. Local widget authoring and ordinary asset
opening use the full editor path.

## 9. Current widget rasterizer: gaps and required enhancements

This chapter is the runtime workstream for the editor. Its changes belong in
shared layout/rasterization/presentation code so the authored result also works
in a packaged experience.

### 9.1 Capability audit

**V1** below denotes the initial editor release, whose new documents use
`layoutVersion = 2`.

| Feature | Current state | Planned work / priority |
| --- | --- | --- |
| Rectangle panel | Implemented. | Preserve legacy rendering; consume resolved version-2 geometry. **V1**. |
| Circle panel | No explicit panel-circle surface; radial sectors and arbitrary polygon surfaces exist. | Add analytic disk coverage, clipping, picking, and containment. **V1**. |
| Explicit element size | Missing. Legacy slot/position heuristics determine extents. | Version-2 boxes, pixel/percentage/defined auto dimensions. **V1**. |
| Anchoring and pivot | Missing for widget elements. The separate `IdtxHud` image path has nine screen anchors. | Shared anchor/pivot resolution; nine inspector presets. **V1**. |
| Percentage sizing | Normalized position exists; percentage element dimensions do not. | Resolve dimensions against the actual containing content box. **V1**. |
| Free arrangement | No explicit `free` container token; legacy normalized positions coexist with slot heuristics. | Explicit absolute-child arrangement using the proposed version-2 boxes. **V1**. |
| Row / Column / Stack | Geometry exists on the radial-containing path; rectangle-only content takes the legacy path. Schema gap/padding/alignment are not hydrated into layout. | One retained hierarchy, mode-specific spacing/alignment, and flow order independent from paint order. **V1**. |
| Grid arrangement | The hierarchical path derives a near-square uniform grid from child count. | Consume Auto/explicit column count, flow membership, gap/padding, and cell alignment in every version-2 panel. **V1**. |
| Table arrangement | No token, track/cell schema, or resolver support. | Add shared column weights, explicit/automatic cell allocation, spans, validation, hydration, rendering, and inspector controls together. **V1**, with the basic scope in section 5.5. |
| Radial arrangement / conversion | Immediate Buttons become sectors; non-Buttons are skipped, per-Button position is ignored, and stick navigation depends on sector geometry. No editor conversion contract exists. | Button-only insertion, container/slot inspectors, arrangement-aware movement, and atomic conversions from section 5.6. Adding/removing the first/last radial subtree must not switch the version-2 resolver. **V1**. |
| Word wrapping / explicit newlines | Missing. `font::draw_text` walks characters on one baseline, and `fit_px` shrinks the line. | Multiline measured text layout, break rules, overflow, alignment. **V1**. |
| Font size policy | Explicit sizes can still shrink to fit; schema/runtime units disagree. | Versioned logical-pixel policy and explicit auto-fit opt-in. **V1**. |
| Font asset selection | Font-family registry exists; schema `idtx:widget:font` is not loaded by widget hydration/rasterization. | Resolve/read/package font assets and add a font fingerprint to cache keys. **V1**. |
| Image fit | `fit` is declared in the schema; the rasterizer currently resizes every loaded image to the entire slot. | Implement fill/contain/cover/none, intrinsic aspect measurement, clipping. **V1**. |
| Slider range | Schema has min/max; current raster fill clamps the raw value to `[0,1]`. | Hydrate range and normalize `(value-min)/(max-min)` with a defined degenerate-range result. **V1**. |
| Visibility / opacity / polygon clips | Implemented, including inherited element opacity and effective visibility. | Preserve semantics; separate hidden editor records from visible paint entries. **V1**. |
| Grid snapping | No widget editor currently exists. | Editor-only geometry operation; rasterizer consumes final authored coordinates. **V1**. |
| HUD-space widget panels | Explicitly skipped by `WidgetRenderer::prepare`; runtime panel interaction also filters to world space. | Screen-space widget compositor plus matching screen-space input routing. **V1**. |
| Texture quality | Resolution/PPU/SSAA/mipmap levers exist, several environment-controlled. | Carry effective raster size separately from logical layout; include quality in presentation/cache policy. **V1**. |
| External `contentSource` | Schema promises imported-document content, but the current rasterizer still iterates native elements. | Expose the actual unsupported state; a real external-document backend is **follow-up**. |
| Advanced styles / text shaping | Solid controls and simple `ab_glyph` text exist. | Theme/state styling, nine-slice images, complex-script shaping/BiDi, and rich text are **follow-up** work. |

### 9.2 Anchoring and responsive layout

Resolve parent boxes before children. Percentages apply to the immediate
parent's content box after padding for absolute children, and the allocated
slot/cell for rectangular flow children. Radial slots derive their geometry from
the container instead. Panel-relative positioning uses a direct panel child or a
full-panel container. Apply pivot correction after resolving the final size.

Use the same resolver for editor, world panels, and HUD. Each supplies a layout
context, rather than teaching each presentation path its own anchor rules.
An intrinsic image aspect ratio is a measurement input, while contain/cover
determines how image content occupies the resolved element box.

**Concrete acceptance example — centered responsive image:**

```usda
# Proposed version-2 properties; root stage metadata is as in section 4.1.
def IdtxWidgetPanel "Panel"
{
    uniform int idtx:widget:layoutVersion = 2
    float2 idtx:widget:designSize = (1920, 1080)
    uniform token idtx:widget:shape = "rectangle"
    uniform token idtx:widget:space = "hud"
    float2 idtx:widget:hud:size = (1, 1)
    uniform token idtx:widget:hud:sizeUnit = "percent"

    def IdtxWidgetImage "Logo"
    {
        asset idtx:widget:image = @logo.png@
        uniform token idtx:widget:fit = "contain"
        uniform token idtx:widget:layout:placement = "absolute"
        float2 idtx:widget:layout:anchor = (0.5, 0.5)
        float2 idtx:widget:layout:pivot = (0.5, 0.5)
        float2 idtx:widget:layout:offset = (0, 0)
        float2 idtx:widget:layout:size = (0.1, 0)
        uniform token idtx:widget:layout:widthUnit = "percent"
        uniform token idtx:widget:layout:heightUnit = "auto"
    }
}
```

For a square source image and UI scale 1:

| Rendered panel | Image size | Image center |
| --- | --- | --- |
| 1280 × 720 | 128 × 128 | (640, 360) |
| 1920 × 1080 | 192 × 192 | (960, 540) |
| 3840 × 2160 | 384 × 384 | (1920, 1080) |

Its width is always 10% of the panel width and its center stays at 50%/50%.
Changing aspect ratio follows the same width/aspect rule. A two-axis percentage
box is also supported when that is the intended design.

Verify that editor zoom, monitor DPI, SSAA, mip selection, and a raster-resolution
cap preserve these logical bounds. A higher-resolution texture improves sampling;
it does not replace responsive layout.

### 9.3 Text layout and wrapping

Introduce a measured text-layout result consumed by rasterization:

1. Resolve font bytes/family and logical font size.
2. Resolve the available content width after padding.
3. Honor explicit newlines and Unicode word-break opportunities; use a defined
   grapheme-safe fallback for an overlong unbreakable run.
4. Compute line advances, baselines, line height, and total measured bounds.
5. Resolve auto height if requested, then vertical/horizontal alignment.
6. Apply clip/ellipsis rules and final element/ancestor/panel shape clipping.

Version-2 wrapping preserves the requested font size unless `text:autoFit` is
explicitly enabled. Cache measurements by text, font fingerprint, size, available
width, and wrapping policy. Selection remains the element's layout shape.

For radial labels, wrap inside the resolved label box and clip to the sector.
Retain legacy single-line fitting for version-1 assets. Complex-script shaping
and BiDi need a separate text-engine increment; avoid presenting simple kerning
as complete shaping support.

### 9.4 Screen-space widget composition

Add the outer HUD-panel contract:

| Property | Meaning |
| --- | --- |
| `idtx:widget:hud:anchor` | Nine screen anchors using the existing token convention; default center. |
| `idtx:widget:hud:size` | `float2`; requested outer panel size, default `(1,1)` with percent units. |
| `idtx:widget:hud:sizeUnit` | `pixels` or `percent`, relative to the logical viewport/safe-area rectangle. |
| `idtx:widget:hud:offset` | `float2`; logical-pixel displacement from the anchored placement. |
| `idtx:widget:hud:order` | Integer screen-space paint order, with stable path tie-breaking. |

Pass viewport dimensions, UI scale, and safe-area information through a shared
`HudLayoutContext`. Resolve the screen rectangle first, then the widget's internal
layout using that rectangle's logical dimensions. World panels continue to use
their design layout and physical quad size.

Refactor texture preparation so HUD panels can obtain rasterized textures before
world/HUD presentation filtering. Extend the existing LDR HUD composition seam
to accept these textures. Define ordering relative to legacy image HUDs explicitly
and use the same ordering for screen-space hit testing.

Route pointer coordinates through the final HUD rectangle into panel-local
coordinates, then invoke the shared widget detail test. HUD consumption precedes
world picking. If runtime HUD controls are interactive, reuse existing widget
events/focus ownership while supplying the new screen-space hit source. Test
that a covered control prevents the click from also reaching a world object.

The editor's resolution preview must exercise this real layout/composition
contract. Gate the inspector's HUD presentation controls on this implementation
being available.

### 9.5 Raster correctness and performance

- Standardize linear/sRGB conversion and straight/premultiplied alpha at image,
  glyph, scratch-buffer, panel-texture, and GPU blend boundaries. Verify transparent
  text/image edges over light and dark backgrounds.
- Rasterize circle edges with the shared shape definition and a documented
  coverage rule. Picking uses the same logical boundary with a defined edge
  tolerance; anti-alias coverage is not an arbitrary selectable rectangle.
- Split layout/paint invalidation from presentation-only changes. World movement,
  editor pan/zoom, hover, and selection overlays should reuse panel content.
- Include logical dimensions, arrangements and resolved slot/cell allocation,
  final layout, text settings, image fit, shape, resolved asset/font revisions,
  effective texture dimensions, and quality in the relevant cache keys. Reloading
  an asset at the same path must invalidate pixels and, when intrinsic dimensions
  change, layout too.
- Recompute affected owning panels on property edits and propagate ancestor
  visibility/opacity/layout changes to dependent descendants. Use an ownership
  index instead of repeatedly scanning the entire stage for every element.
- Retain the existing per-axis raster ceiling and budget total cache/scratch
  memory. The current rasterizer clears a full-panel scratch buffer per element;
  reuse buffers and clear/composite bounded dirty regions where profiling shows
  this affects dragging.
- Keep editor outlines, grid, rulers, and gizmos in the overlay path so these
  interaction changes never rerasterize widget content.

## 10. Additional editor best practices

| Feature | Delivery |
| --- | --- |
| Fit/100%/selection framing, checkerboard, rulers, panel bounds | Initial release; make logical sizes and transparent content understandable. |
| Undo/redo, Escape cancellation, numeric editing, keyboard nudging | Initial release; share gesture and validation code with the gizmo. |
| Anchor/pivot presets, unit conversion, percentage/intrinsic sizing | Initial release; use shared runtime layout. |
| Arrangement presets, slot/cell editing, validated conversions | Initial release; one Container schema, including basic Table and Button-only Radial authoring. |
| Resolution/aspect presets and safe-area guides | Initial release alongside HUD composition. |
| Hidden-element outline recovery and selection lock | Initial release; editor lock is distinct from native runtime visibility or enabled state. |
| Asset/font status and compute-binding readout | Initial release; inspector shows authored inputs and resolved results. |
| Resize handles with aspect lock | Follow-up; reuse layout dimensions and full-footprint constraints already used by numeric size editing. |
| Multi-selection, alignment/distribution, marquee selection | Follow-up; expand the current single-selection model and constrain group movement as a whole. |
| Duplicate/copy/paste and image drag/drop | Follow-up through validated widget commands; preserve relative asset paths and remap internal USD relationships. |
| Smart guides / sibling snapping | Follow-up on the same snapping interface as the fixed grid. |
| Explicit Edit / Interact preview mode | Follow-up; interactive preview uses runtime events and input capture, with a clear mode boundary. |
| Reusable styles, control states, nine-slice images | Follow-up shared schema/rasterizer work before adding inspector controls. |
| Accessibility checks and keyboard focus visualization | Follow-up: tab order, activation, contrast, target size, and disabled-state behavior, using real runtime focus semantics. |
| Localization and complex-script preview | Follow-up text-engine work: shaping, BiDi, fallback fonts, and language-sensitive breaking. |
| Polygon outline authoring and advanced radial tools | Follow-up; independent sector transforms require an extended geometry/navigation contract. Numeric radial settings and whole-container movement are in the initial release. |
| Advanced table tracks and data-table behavior | Follow-up: fixed/intrinsic track sizing, headers, repeated data-bound rows, sorting, and virtualization on top of the shared-cell layout. |

Version-2 managed layout uses `layout:flowOrder` independently from paint order,
with the compatibility fallback defined in section 5.2. Slot/cell sequence and
runtime navigation consume that sequence; bring-to-front changes only painting.
Before editing paint order on an element still using the fallback, materialize
its current flow order in the same transaction to keep slots/navigation stable.
Keep editor preferences in workspace/document UI state; serialize only layout or
behavior properties that the rendered widget consumes.

## 11. Implementation work packages

### 11.1 File-level change map

| Area | Existing files / proposed additions |
| --- | --- |
| Stage taxonomy | `crates/idtx-project/src/lib.rs`; extend `StageKind` and classification tests. |
| Live root metadata and typed properties | `crates/idtx-usd/src/lib.rs`, `src/stageservice.rs`, `src/stageservice/structural.rs`; `crates/idtx-usd-sys/src/bridge.rs`, `include/usd_shim.h`, `cxx/usd_shim.cpp`. |
| Widget schemas | `crates/idtx-usd-extension/schema/schema.usda`, `resources/generatedSchema.usda`, `resources/plugInfo.json`, `src/lib.rs`, and schema tests. |
| Authored and resolved widget data | `crates/idtx-scene/src/components.rs`, `hydrator/widget.rs`, `widget_layout.rs`, `widget.rs`; add `widget_picking.rs` and focused arrangement, table allocation, conversion, and constraint helpers as needed. |
| Picking reuse | `crates/idtx-scene/src/picking.rs`; extract reusable slab/plane/UV helpers; optional later use of `collision_bvh.rs` mechanics for the widget index. |
| Rasterization and text | `crates/idtx-render/src/widget_pass.rs`, `font.rs`; add a text-layout module and preview/composition seam. |
| Render extraction and HUD | `crates/idtx-render/src/scene_extract/snapshot.rs`, `scene_extract/extract.rs`, `hud.rs`, and renderer presentation integration. |
| Session integration | `crates/idtx-studio-session/src/lib.rs`, `scene_backend_impl.rs`; add focused `widget_editor.rs` / `widget_authoring.rs` modules for preview, selection, and commands. |
| UI-neutral data | `crates/idtx-editor-model/src/lib.rs`; add `widget.rs` for DTOs, arrangement capabilities, template IDs, policies, and intents. |
| Canvas UI | Add `crates/idtx-editor-ui/src/widget_editor/` for state, canvas painting, toolbar, and gesture intents; register in `src/lib.rs`. |
| Inspectors | Add widget contributions under `crates/idtx-editor-ui/src/inspector/panels/`; extend registry, common controls, and edit-sink vocabulary. |
| Tree UI | `crates/idtx-editor-ui/src/scene_tree.rs`; widget-only policy, typed Add submenu, arrangement badges, and concrete templates using shared parent/child validation. |
| Shell wiring | `crates/idtx-studio-shell/src/model.rs`, `view.rs`, `projection.rs`, `tabs.rs`, `actions.rs`, `render.rs`; add `widget_tools.rs` to keep specialized dispatch out of the central render function. |
| Undo | `crates/idtx-authoring/src/lib.rs`, `crates/idtx-studio-shell/src/authoring_ops.rs`; grouped, target-aware captures and replay. |
| Packaging / documentation | Widget fixtures/examples, Conductor README, schema usage docs, and cook-side validation in `crates/idtx-build` where widget runtime compatibility/assets are checked. |

### 11.2 Ordered delivery plan

The size labels are relative implementation scope, rather than calendar estimates.
The full initial release includes all work packages below.

| Package | Scope / dependencies | Exit criteria | Size |
| --- | --- | --- | --- |
| **W0 — Contract and authoring foundations** | Add stage taxonomy/live metadata; finalize version-2 coordinates, Free/Table tokens, grid/table attributes, flow order, and arrangement capabilities; complete typed vectors/arrays and target-aware transactions. | Existing tagged fixtures classify correctly; all tokens/defaults/types round-trip, including table weights/cells; absent opinions and sibling metadata survive edits. | M |
| **W1 — Shared layout and shapes** | Depends on W0. Add explicit boxes, hierarchy, all seven arrangements, table occupancy/tracks/spans, anchors, percentage/auto sizing, rectangular managed offsets, circle geometry, conversion planning, derived bounds, and legacy migration. | Geometry tests prove responsive placement, every arrangement's allocation, full-shape containment, and conversion eligibility. First/last radial changes leave unrelated version-2 siblings stable. Legacy fixtures retain baseline geometry before upgrade. | L |
| **W2 — Rasterizer parity** | Depends on W1. Consume resolved snapshots; implement wrapping/font assets, image fit, range-correct controls, quality-coordinate separation, and shared texture preparation. | Pixel/geometry tests agree for Rectangle/Circle/polygon clips, wrapped text, image fits, and transparent edges. | L |
| **W3 — Automatic orthographic editor** | Depends on W0 and the W1/W2 preview seam. Add center tab, activation helper, active-panel discovery, canonical view, toolbar, per-document state, and preview textures. | Open/reactivate/reload selects Widget Editor; pan/zoom/DPI tests pass; 3D/canvas state stays isolated; empty/multiple-panel documents behave predictably. | M |
| **W4 — Picking and movement** | Depends on W1/W3. Add widget query, exact-path selection, arrangement-aware XY gizmos, slot/cell overlays, snapping, nudges, full-footprint constraints, and grouped gesture undo/cancel. | Overlap picks select the top element; disabled controls remain editable; circle dragging stays inside; radial slots expose container-based positioning; moving the container keeps pixels/picking/navigation aligned; one drag produces one undo entry. | L |
| **W5 — Inspector and tree authoring** | Depends on W0/W1/W4. Add independent panel-shape/container-arrangement inspectors, atomic conversion commands, typed Add submenu with parent filtering, Table/Radial controls, creation templates, new-stage action, and validated add/remove commands. | Every supported attribute previews; shape/arrangement combinations work; conversions follow section 5.6; Radial offers only Button insertion; Field choices create concrete typed prims; structural/conversion undo restores opinions and content. | L |
| **W6 — Responsive HUD presentation** | Depends on W1/W2 and HUD inspector controls from W5. Implement outer HUD placement, real screen-space composition, resize invalidation, and input routing. | The centered-image example passes at three resolutions and changed aspect ratios; HUD picking matches pixels and consumes input before world picking. | L |
| **W7 — End-to-end integration** | Depends on W3–W6. Complete arrangement/conversion/navigation, migration/save/package regressions, feature-build checks, profiling, examples, and user documentation. | All release acceptance scenarios pass with a real USD link and GPU where required; a packaged runtime understands the new arrangements and renders/interacts with their authored results correctly. | M |

The critical dependency chain is typed authoring → shared layout → rasterizer /
picking → gestures and inspectors. Treat responsive HUD composition as a release
dependency for the HUD promise, rather than claiming completion from an editor
resize preview alone.

## 12. Validation and acceptance plan

### 12.1 Automated coverage

| Track | Meaningful checks |
| --- | --- |
| Metadata / activation | Root-only tag; token and string values; non-widget level referencing a widget; missing/unknown tag; live tag changes; existing-tab browser activation; reload; manual center-tab switching; no cross-document state write-back. |
| Schema / authoring | Every new type/token/default, including Free/Table; float2/float arrays/int2 exact types; flow-order authoredness/fallback; correct uniform declarations; root/session/variant targets; edit then clear restores absence; unrelated metadata and opinions survive. |
| Layout | Explicit `(0,0)` placement; all nine anchors/pivots; pixel/percentage conversions; nested padding/alignment; image aspect/auto size; wrapped-text measurement; cyclic auto-layout diagnostics. |
| Arrangements | All seven modes in Rectangle and Circle panels; nested modes; absolute overlays excluded from flow; Grid Auto/fixed columns; gap/padding/alignment; flow versus paint order; disabled radial slots; first/last radial subtree changes preserve unrelated version-2 geometry. |
| Table allocation | Shared weighted columns across rows; mixed Label/Field/Image/Button cells; explicit holes; deterministic auto placement; spans; hidden/composed cell occupancy; conflicting assignments; invalid counts/weights; cell-relative percentages/wrapping; insertion/removal and grouped track-edit undo. |
| Arrangement conversion | Every allowed/blocked transition in section 5.6; empty and populated containers; incompatible child types, including hidden/referenced ones, block Radial conversion; all-Button Row ↔ Radial preserves identity/content but changes shape; radial-to-Free is explicitly rectangular; inactive settings survive save/reopen; exact undo; legacy migration and conversion are atomic. |
| Shape / constraints | Rectangle edges; circle corners and edge tangency; oversized elements; non-square raster targets; circle-axis dragging; boundary sliding; container descendants; polygon holes and radial sector extrema; a matching-diameter radial menu fits a Circle despite allocation-frame corners. |
| Picking | Topmost overlap and equal-order path ties; container/background fallback; clipping; hidden/disabled states; exact element identity through references; parallel rays; pan/zoom/DPI; stale bounds after edits; deletion during a queued pick. |
| Gestures / history | Pointer-down baseline retained across hydration; snap/bypass/nudge; one drag/one undo; no-op drag; Escape/focus loss/document switch; group failure rollback; target-layer replay; conflicting edits preserve unrelated changes; radial slot XY controls are unavailable and whole-container movement is coherent. |
| Runtime navigation | Whole-menu translation/resizing; radial gaps/dead zone/hysteresis and disabled slots; conversion into/out of Radial changes stick capability; cyclic navigation follows flow order independently from paint order; stale focus/highlight clears; path-addressed activation bindings survive. |
| Rasterization | Pixel tests for circle/polygon coverage, line wrapping/newlines/overflow, image fit/aspect, font asset fallback, and transparent-edge blending; geometry matches hit samples. |
| Responsive HUD | 1280×720, 1920×1080, 3840×2160, portrait/changed aspect, UI-scale variation, viewport resize, clipping, screen order, and pointer mapping; HUD consumes covered input before world selection. |
| Structural policy | Exact two top-level authoring entries when valid; full Field/type/Container submenus; correct concrete schema/template per choice; Button-only Radial submenu; invalid/leaf/helper parents; protected panels; stale intent after arrangement change; composed-element removal; undo subtree deletion; forbidden generic context-menu/keyboard/paste intents; collaboration capabilities. |
| Persistence / compatibility | Open/save legacy fixtures without migration; upgrade/undo; `.usda` and `.usdz` save/reopen; two references to the same asset with different overrides; image/font packaging; layout version behavior in the packaged runtime. |
| Invalidation / cost | Selection/hover/pan/zoom leave raster content cached; geometry edits refresh bounds and pixels together; same-path asset reload; unrelated panels stay cached; repeated tab open/close releases textures. |

Use pure geometry/constraint tests for mathematical behavior, real-USD tests for
composition and authored-opinion restoration, and headless GPU/readback tests
for presentation. Pixel tests should check meaningful coverage and alignment
invariants rather than relying exclusively on whole-frame screenshots.

### 12.2 Existing checks to extend

Run from `idtx-studio/` with the workspace's documented OpenUSD environment. GPU
acceptance runs require an actual adapter; a skipped render test is not evidence
of presentation correctness. These are implementation verification commands,
not checks executed as part of writing this plan.

```bash
cargo test -p idtx-project stage_kind
cargo test -p idtx-usd --features authoring --test stage_kind
cargo test -p idtx-usd-extension --lib
cargo test -p idtx-usd-extension --test widget_schema -- --include-ignored
cargo test -p idtx-scene --lib widget
cargo test -p idtx-scene --test widget_shapes
cargo test -p idtx-render --lib widget
cargo test -p idtx-render --test widget_render -- --include-ignored
cargo test -p idtx-studio-session --features authoring --test behavior_widget -- --include-ignored
cargo check -p idtx-conductor
cargo check -p idtx-studio-shell --features authoring
cargo check -p idtx-studio-shell --no-default-features
cargo check -p idtx-scene --no-default-features
git diff --check
```

Add focused widget-editor suites in `idtx-editor-ui`, `idtx-studio-shell`, and
`idtx-studio-session` for the new canvas, menus, arrangement conversion, transaction,
and activation paths. Extend runtime/backend tests for whole-container radial
movement, arrangement-dependent navigation, table layout, and HUD event routing.
Run the existing readability suite when changing quality/font-size behavior, and
verify the target runtime builds that consume the shared renderer/schema resources.

### 12.3 Manual release walkthrough

1. Create a widget stage. Confirm the root tag, default panel, automatic Widget
   Editor tab, orthographic presentation, and panel inspector.
2. Open **Add Widget Element** and add a Label, Button, Image, each Field type,
   and a nested Container preset. Verify concrete USD types; select the elements
   both in the tree and by clicking their rendered locations.
3. Overlap two elements and change order. Confirm that the visible top element
   receives selection and that empty panel space selects the panel.
4. Move elements with each gizmo handle, snapping, and keyboard nudges. Test all
   rectangle edges; switch to Circle and test full-box containment and boundary
   sliding. Undo, redo, and Escape a drag.
5. Exercise Row, Column, Grid, Stack, and Free on both panel shapes. Confirm that
   changing Surface Shape does not choose an arrangement and that container
   placement is independent from its child layout.
6. Create a Table with Label/Field cells, unequal column weights, an explicit empty
   cell, and a span. Check shared column alignment, cell editing, validation, and
   undo after changing column count or inserting/removing a cell.
7. Create a Radial container and add Buttons through its filtered submenu. Check
   slot sequence, disabled slots, radial settings, and whole-container movement.
   Convert a Button-only Row to Radial and back; check navigation and exact undo.
   Attempt conversion of a mixed Row and confirm the incompatible paths are
   identified. Verify the explicit rectangular result of Radial → Free.
8. Center a percentage-sized image, add wrapped text, and preview multiple
   resolutions/aspect ratios. Confirm the chapter-9 example numerically and in
   the real runtime HUD presentation.
9. Switch to 3D View, activate another document, then reactivate the widget stage.
   Confirm automatic canvas activation and preserved per-document view state.
10. Inspect all context-menu targets while both center views are active. Confirm
    the two valid widget-only authoring entries, typed submenus, Radial filtering,
    and protected panel roots. Change a parent arrangement before dispatching a
    queued Add intent and confirm it is revalidated.
11. Save/reopen loose and packaged assets, reopen the legacy fixtures, and preview
    the widget referenced twice in a normal level. Confirm all arrangements,
    assets, picking, inactive mode settings, and instance-specific overrides.

## 13. Definition of done

The feature is complete when a Conductor user can create/open a root-tagged
widget stage, immediately edit it in the orthographic tab, choose Rectangle or
Circle independently from a container's Free/Row/Column/Grid/Table/Stack/Radial
arrangement, author supported properties, and select the element actually under
the pointer. Movable elements and whole radial containers stay within their owning
surface; slot/cell editing and permitted arrangement conversions support undo.

The scene-tree menu must enforce widget-only add/remove authoring with a concrete
type-selection submenu and parent-specific choices. Arrangement conversion must
preserve element identity/content, reject incompatible Radial children, and make
sector-to-rectangle changes explicit. Anchors, percentage sizes, wrapping, and
snapping must produce persistent authored results, and the same layout must render
correctly in the packaged world/HUD paths. Legacy radial/polygon assets, typed
value bindings, navigation, and save/reopen behavior must pass their regressions.
