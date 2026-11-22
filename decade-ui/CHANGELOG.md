# Changelog

All notable changes to `decade-ui` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not.

## [Unreleased]

### Added

- `Scene` is a list of shapes in pixels with helpers for a grid of
  monospace cells. `Shape::Content` marks where a program draws what a
  `ContentId` shows. `CellRect`, `Point` and `Size` are its geometry, and
  `palette` holds `Rgba` and the theme's colours.
- `Fonts` loads the chrome and menu fonts at a size in pixels and
  rasterizes a run of text in a `Face` to premultiplied RGBA pixels.
  `fit` and `tail` shorten text to a number of characters.
- `Layout<A>` is a split tree of panes, each with tabs of content ids,
  a bar and the `Marks` a program gives it: buttons holding its actions,
  tags and an empty pane's placeholder. `press`, `motion` and `release`
  resize, move and focus panes and return what the program must do.
- The `overlay` module holds the parts every overlay box is drawn from,
  with `MenuKey` and `Outcome<A>`. Its widgets are `ContextMenu<A>`,
  `Dialog<A>` and `NameField<A>`. A context menu entry may ask for a
  second click before it runs.
