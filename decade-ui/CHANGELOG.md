# Changelog

All notable changes to `decade-ui` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not.

## [Unreleased]

### Added

- `Chrome::driven` names the pane an agent's input plays in, as a
  `Driven`. The layout mixes the pane's outline toward yellow by its
  `strength` and draws a yellow tag after the recording tag with its
  `icon` and `label`.
- `Chrome::recordings` lists the panes being recorded, each a
  `Recording`. The layout mixes the pane's outline toward red by the
  recording's `strength`. It draws a red tag after the left tag with the
  recording's icon while `lit` and its `elapsed` text.

- `Layout::hover_focus` gives the pane that pointer motion onto a cell
  focuses, or `None` during a drag, over a shown Ctrl bar or on the
  focused pane.
- `Layout::pointer_target` gives the middle of the focused pane in
  pixels, or `None` when a given cell is on that pane already.
- `MenuKey` has `Home` and `End`. `NameField` keeps a caret that Left,
  Right, Home and End move, and typing and Backspace act at it. The first
  key still replaces the old name unless it is a movement key.
- `Scene` is a list of shapes in pixels with helpers for a grid of
  monospace cells. `Shape::Content` marks where a program draws what a
  `ContentId` shows. `CellRect`, `Point` and `Size` are its geometry.
- `palette::Palette` holds the colours of an Omarchy theme and shades
  blended from them. `Palette::from_theme` reads them from the text of a
  `colors.toml`, with a fixed colour for each key it lacks. A program
  sets one with `palette::set` at start, and drawing code reads it with
  `palette::get`.
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
