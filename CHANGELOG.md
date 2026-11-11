# Changelog

All notable changes to decade are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not. What is versioned is the
command line and what the tool writes.

## [Unreleased]

### Added

- A server holds workspaces. Each has a name, a working directory and its
  own panes. A new server starts one workspace in the home directory, named
  after that directory, with one terminal. Programs decade starts run in
  their workspace's directory, with its name in `DECADE_WORKSPACE`. Each
  workspace has its own socket in `DECADE_SOCKET` and its own MCP address
  in `DECADE_MCP`. A client or an agent reaches only the panes of its own
  workspace, and a pane name means a pane in that workspace.
- New workspace in the palette takes a path, starting at `~/`, and lists
  the directories that complete it. Tab or Enter on a directory completes
  it, and Open creates a workspace there and shows it. The workspace takes
  the directory's name, with a count after it when that name is taken.
- A top bar two rows tall replaces the status line. On the left it shows the
  workspace's name in bold, a pill for each tab and the focused window's
  title. A tab's pill holds the name of what runs in the tab's focused pane.
  For a terminal decade started, that is the shell's foreground program. At
  the prompt it is the name of the shell's working directory. For another
  window it is the last part of the app id. A pane that holds a connected
  agent takes the agent's name instead, on the top bar and on the first of
  the pane's own tabs. A dot on the pill in a state's colour means a
  connected agent in one of the tab's panes has reported that state. On the
  right, a pill for each of needs you, done and working counts the
  workspaces in that state. A click on one shows the next workspace in that
  state. The last button opens the palette. A sidebar on the left lists
  every workspace as a card, those whose agents most need the person first.
  A card shows the workspace's name, its directory and its git branch, with
  a disc at its top right holding the mark of the agent's state. Under them
  come the agent's name with its state and how
  long it has held that state, the agent's question or activity, and the
  model, context use and tokens the agent has reported. The model and
  tokens are dim. The context use is green under 50%, yellow up to 80% and
  red above. When a
  workspace has several agents, its card shows the one that most needs the
  person, with +N for the others. decade draws an agent that has
  disconnected or lost its pane in grey, and leaves it out of the state
  counts and the sidebar's order. A click on a card shows its workspace.
  When the agent is done or needs attention, the disc under the pointer
  gets a ring and a check mark. A click on that disc returns the agent to
  idle. The background of such a card fades toward a dark tint of its
  state's colour and back every 1.6 seconds. Super+B
  collapses the sidebar to a rail of tiles. Each tile shows a workspace's
  initial. A dot on its corner takes the colour of the agent's state. A last
  tile marked + starts a new workspace. With the pointer on a tile, a card
  beside the rail lists each of the workspace's agents with its state and
  its question or activity. The pills on the top bar and the cards in the
  sidebar brighten under the pointer. Super+A shows the next workspace whose
  agent needs the person.
- The server saves its workspaces to `~/.local/state/decade/workspaces.toml`
  and restores them when it starts. The file holds each workspace's name,
  directory and split tree, and each pane's name and whether it is pinned.
  A terminal pane comes back as a shell in the directory its shell was in.
  A pane that held another app comes back empty, and a window of that app
  fills it again. decade saves a second after a change, every minute, and
  when it stops.
- A socket client that runs an agent adds an `agent` block to hello, with
  the agent's name, version and session. It reports on the agent in
  its pane with `agent` messages, which carry any of a state (attention,
  done, working or idle), an activity, a question, the model, context
  use, token counts and cost. decade merges each message into what it
  holds. A field left out keeps its value. A field set to null is
  cleared. A state holds until its client or the person at the screen
  replaces it. Done is the exception. When a viewer shows a workspace,
  decade starts a 10-minute clock on each of its done agents. When the
  clock runs out, the agent returns to idle. A report from the agent stops
  its clock. The clock keeps running when the person switches away.
  decade shows the question beside the state, or the
  activity when there is no question. A pane entering attention raises a
  desktop notification unless a focused viewer shows its workspace.
  decade keeps each workspace's agents in memory, by session. When an
  agent's connection drops, decade keeps its report and its last 32 state
  changes and marks it disconnected. A later hello that names the session
  attaches to the agent again and moves it to the pane that hello claims.
  decade drops an agent 10 minutes after it disconnects, or at once when
  its client sends `ended`. The MCP tool `agents` lists the workspace's
  agents with their pane, state, age, question, activity, model, context
  use, tokens, cost and whether each is connected. `decade-channel` gains
  `AgentState`, `Agent`, `AgentUpdate`, `Channel::agent` and
  `Channel::ended`.
- Super+Tab opens a picker with a cell for each workspace, showing its
  panes scaled down, and a last cell for a new workspace. Super+Tab, Tab,
  the arrow keys or Ctrl+H/J/K/L move between cells. Enter or a click
  shows the workspace, and Esc closes the picker. Dragging a cell moves it
  in the order. The x on a cell closes its workspace after a second press.
  While the picker is open, programs in workspaces no viewer shows draw
  twice a second, so their cells stay current.
- Close workspace in the palette asks for a second Enter, then closes every
  pane in the workspace. The workspace goes once its last client has, and
  viewers showing it move to the first one left. Closing the last workspace
  starts a new one in the home directory.
- A workspace holds tabs, each its own split tree of panes. Super+T opens a
  tab with one terminal in the workspace's directory and shows it. Super+[
  and Super+] show the previous and next tab. They wrap at the ends. The
  top bar shows a numbered pill for each tab after the workspace name,
  with the shown tab filled, and a `+` pill that opens a tab. A click on
  a tab's pill shows that tab. Programs in hidden tabs keep running at
  the size of the pane area. decade sends them no frame callbacks until
  their tab is shown. A pane name stays unique across a workspace's tabs.
- Close tab in the palette asks for a second Enter, then closes every pane
  in the shown tab. The tab goes once its last client has. Closing the last
  tab closes the workspace.
- A tab closes by itself once no pane in it holds a client or a running
  program, and none is pinned or the scratch pane. A workspace's only tab
  stays, with an empty pane.
- `workspaces.toml` is now version 4. It holds each workspace's tabs, the
  name the person gave each tab, the tab each workspace shows, whether
  the sidebar is open, and the sidebar's groups with their members and
  folds. decade still reads a version 3 file, which has no groups, and a
  version 2 file, whose tabs have no names either. It reads a version 1 file as one tab per workspace with
  the sidebar open.
- Panes are a split tree. A new window with no anchor of its own, or
  Super+Shift+Return, splits the focused pane along its longer side; a pane
  made that way closes when its client exits.
- Holding Ctrl alone for 50 ms shows a bar over the top row of each pane
  with its name and buttons to pin, zoom and close it. The focused pane's
  name is bold. A button under the pointer sits on a rounded square. The
  close button's square is red. Dragging the outline between two panes
  resizes them.
- Pinning a pane makes it an anchor for its program's app id. An anchor
  stays as an empty slot, saying what it waits for, when its client goes,
  and takes it back when it returns. A pinned terminal whose shell exits
  shows a restart button instead, and a click on the slot or Enter starts
  a new shell in the pane's directory. Unpinning an anchor makes it a pane
  that closes with its program.
- A button on the Ctrl bar marks a pane as its workspace's scratch pane.
  Agents show things there. A workspace has one scratch pane at most.
  Marking a pane unmarks the one before it. The scratch pane carries a
  scratch tag on its outline. It stays as an empty slot when its client
  exits. The x on an empty scratch pane removes it. `workspaces.toml`
  records which pane is the scratch pane.
- When an agent calls the MCP tool `scratch.open`, decade starts a program
  in the scratch pane. The program runs in a terminal or as its own window.
  `program` is a name on decade's `PATH`, or a path relative to the
  workspace's directory. decade asks a program already in the scratch pane to close
  first. For a program that runs as its own window, the call waits for the
  window, for the program to exit, or for 5 seconds. decade answers with
  which of these happened. After an exit, the answer gives the exit status
  and the last 20 lines of the program's stderr. The optional `wait` sets
  how many seconds the call waits. When a program is still running at the
  end of the wait, decade puts its window in the scratch pane once the
  window opens. For a program in a terminal, decade answers that the
  terminal started. The MCP tool `scratch.send` sends a message to the
  program in the scratch pane.
- Ctrl+dragging a pane, or dragging it by the name on its bar, carries a
  small copy of it under the pointer. Dropping it on a target drawn over
  another pane swaps the two or splits that pane on one side. Dropping it
  on a window edge makes it span that side.
- Super+Shift+Space opens a command palette with Launch and Settings. Typing
  filters the list, the arrow keys move, and Esc goes back or closes it.
  Launch lists the installed GUI apps and starts the chosen one as a new
  pane.
- Settings live in `~/.config/decade/config.toml`. Settings in the palette
  opens a panel that changes them and saves the file.
- Scroll wheel events reach clients.
- Menus, popovers and tooltips that a program opens appear over its pane,
  kept inside the pane, and take the pointer and keyboard while they are
  open. Before, a GTK program such as ghostty froze on its first menu. decade
  closes them before it acts on a click on its own chrome, a click on
  another pane or a decade key such as Super+H. Super+C, Super+V and
  Super+X still go to the open menu.
- decade is an MCP server over streamable HTTP, at the address in
  `DECADE_MCP` in the environment of everything it starts. Its tools are
  `point`, `panes`, `show`, `highlight` and `clear`; a call becomes
  the matching socket message and decade's answer is the tool result.
  `show`, `highlight` and `clear` name a kind, such as `file`, and go to
  the pane whose program shows that kind, or to the pane the call names.
  `panes` lists the kinds each pane's program shows, and marks the
  scratch pane and the pane the question came from. A program started
  in a pane gets that pane's name in its `DECADE_MCP` address, so an
  agent it runs finds its own pane.
  With `point` an agent points at a pane. Keyboard focus stays where it
  is. decade pulses the pane's outline in magenta and shows the
  agent's note on it. When the pane's tab is hidden, its pill on the top
  bar pulses. When the workspace is hidden, its card or tile pulses. The
  point ends when the person focuses the pane, after 30 seconds, or at
  the agent's next point.
- A unix socket for decade-aware programs, with its path in `DECADE_SOCKET`
  and the pane name in `DECADE_PANE` of everything decade starts. JSON lines:
  a hello that claims a pane and lists the kinds the program can show,
  messages addressed to a pane and forwarded to its occupant, the standard
  verbs `show`, `highlight` and `clear`, `panes` for decade itself, and an
  error line for anything else. `docs/protocol.md` has the details.
- `decade start`, `decade stop`, `decade restart` and `decade status`
  control the server. `start` runs a server in the background and opens no
  window. `stop` ends the server and every program in its panes. `status`
  prints the server's pid, and exits with 1 when no server is running.
  `decade list` prints each workspace's id, name, number of panes and
  directory.
- `decade script FILE` drives a running server from a script and saves the
  frames it draws as PNG files, for checking the UI without a window. It
  connects as a viewer and runs one command per line: `key super+tab`,
  `type text`, `press ctrl` and `release ctrl`, `hold ctrl 400`,
  `move x y`, `click x y`, `right-click x y`, `drag x1 y1 x2 y2`,
  `size width height`, `wait ms` and `shot file.png`. Lines starting with
  `#` are comments.
- `--class <app id>` sets the window's app id, for window rules. The default
  is `decade`.
- Super with h, j, k or l moves focus between panes by direction,
  Super+Shift swaps the focused pane with its neighbour, Super with an arrow
  key moves the nearest divider, and Super+M zooms the focused pane. A click
  focuses the pane under the pointer.
- Held keys repeat in decade's own bindings, the palette and the settings
  panel, at the delay and rate of the compositor decade's window is on.
- Under Hyprland, while decade's window is the only one on its workspace,
  Hyprland passes decade the keys it would otherwise take. Binds marked
  `dont_inhibit` keep working in Hyprland.
- A pane holds tabs, and each tab holds one program. Super+Shift+T starts
  a terminal as a new tab of the focused pane. The terminal starts in the
  pane's directory, or in the workspace's when the pane has none.
  Super+Shift+[ and Super+Shift+] show the previous and next tab. A pane
  with more than one tab shows a row of rounded pills at its top. Each pill
  shows what runs in its tab, by the same rule as the top bar's pills.
  decade cuts the labels when the pills would not fit. A click on a pill
  shows that tab. When a tab's program exits, decade shows the tab
  beside it. The x on the Ctrl bar closes the tab shown. decade saves the
  directory of each terminal tab and starts those terminals again.
- A right click on decade's own chrome opens a menu at the pointer. A
  pane's bar or outline offers Pin, Zoom, Mark as scratch and Rename. Each
  entry is named for what a click on it does, so a pinned pane offers
  Unpin. A workspace's card or tile offers Rename, and Clear when its agent
  is done or needs attention. A tab's pill offers Rename. Up and Down move
  the selection, Enter runs the selected entry and Escape closes the menu.
  A click on an entry runs it. A click outside closes the menu and goes no
  further. A right click inside a program's window still goes to that
  program. A right click on empty space in the open sidebar offers New
  group. A pane's menu also offers Accept screenshots.
- The person can rename a workspace, a tab or a pane. Rename in the right
  click menu opens a text field over the name, and so does a double click on
  the name. The field's text starts where the name was drawn on the
  workspace's card or in its tile's popover, on the tab's pill, or on the
  pane's bar. The first key replaces the old name. Enter keeps the new name.
  Escape or a click elsewhere cancels. decade refuses an empty name. It
  refuses a name that another workspace holds, that another tab of the
  workspace holds, or that another pane of the workspace answers to. A pane
  name may not hold `&`, `=` or a space. When decade refuses a name, the
  field stays open with the reason under it. A tab with a name shows it on
  its pill in place of what runs in it. A renamed pane answers to its former
  names for as long as it lives. A message, a hello or an MCP call that
  gives a former name reaches the pane, and so does a terminal started under
  that name. `panes` lists only the current name. A new pane cannot take a
  former name.
- The person can put workspaces in named groups in the sidebar. New group
  asks for the group's name in a text field at the bottom of the list. The
  new group goes below the others. Ungrouped workspaces come first. Each
  group follows under a header with its name. Its cards are ordered by
  agent state like the ungrouped ones. Dragging a card onto a group moves
  the workspace there. Dragging it above the first header takes it out of
  its group. A card's menu also offers a move to each other group and out
  of its own. Dragging a header moves its group among the others. A click
  on a header folds the group to its header. A folded header counts the
  group's workspaces in each state, such as `! 1  ✓ 2`. It breathes like a
  card while one of them is done or needs attention. A header's menu
  offers Rename and Delete. Deleting a group returns its workspaces to
  ungrouped. The closed sidebar draws a thin rule between one group's
  tiles and the next. Super+A and the state counts on the top bar cover
  every group. A click on a card in the open sidebar now acts on release,
  so that a press can start a drag.
- The MCP tool `screenshot` takes `pane` and answers with a PNG of the
  pane's client area, at the screen's pixels and without its bar or outline.
  decade refuses a pane in a tab that is not shown. Each pane has an accept
  screenshots mode, which starts off and is never saved. The person turns it
  on and off with a button on the Ctrl bar or from the pane's right click
  menu. With the mode off, decade opens a dialog in the middle of the window
  on every viewer, dims everything behind it, and points at the pane. The
  dialog names the agent, the workspace and the pane. It lists allow once,
  allow and turn the mode on, and deny, with the first selected. Up and Down
  move the selection and Enter answers with it. A click, or y, a or n,
  answers at once. Escape denies. While the dialog is up, no key or click
  reaches a pane, the chrome or a keybind. Keys and clicks in its first half
  second do nothing. A deny, or no answer within 30 seconds, fails the call
  with the reason. A pane with the mode on carries a camera tag at the left
  end of its outline's top edge. The tag flashes at each capture. When a
  pane is too narrow for every tag, the camera tag is the last to go. On
  each capture decade also lays a white layer over the pane's client area.
  The layer starts at 30% opacity and fades out over 300 ms. decade takes
  the PNG before it draws the layer, so the PNG never shows it.
- A program's hello may give each verb in `accepts` a description of one
  sentence, as an object with `name` and `description` in place of the
  bare name. `panes`, on the socket and over MCP, lists each pane's
  `accepts` beside `shows`. The description of the MCP tool `scratch.send`
  names `panes` as where the agent finds the verbs the program in the
  scratch pane takes.

### Changed

- decade runs as two parts. `decade server` owns the panes and their
  programs and has no window. `decade` opens a window that shows the server
  and sends it input, and starts a server first if none is running. Closing
  the window leaves the server and its programs running, and running
  `decade` again shows them again. The two talk over the protocol in
  `docs/viewer.md`.
- Copy and paste work between panes, and between panes and the host, for
  both the clipboard and the primary selection. Super+C, Super+V and
  Super+X copy, paste and cut in the focused pane.
- A pane is drawn as a box-drawing outline, one stroke wide, in cyan on the
  focused pane and a muted colour elsewhere; the title strips are
  gone. Colours come from a palette copied from the Omarchy theme.
- decade is a Wayland compositor nested in a window, on smithay. Any Wayland
  client is a pane; the terminal pane runs ghostty. Clients are told the
  host's scale and render at it. decade draws its own chrome on the GPU.
- The window opens on a headless Hyprland output during development; see
  `scripts/devscreen`.
- Clients in panes render on the same GPU as decade.
- The palette and the New workspace browser share one width and one top
  edge. The palette is as tall as its rows. A dim scrim covers the window
  behind the palette, the settings panel and the picker. Each of them ends
  with a line of key hints. Each key's name sits in a small key cap. The
  panel shows each value in a rounded pill.
- decade draws its own overlays from one set of parts in two sizes. The
  palette, the settings panel and the screenshot dialog take the large size,
  in the palette font. The right click menu, the name field and a tile's
  popover take the small size, in the chrome font, beside what they belong
  to. Each is a rounded box with a grey border and a shadow. Its corners
  grow with the font. A list's selected row has a highlight and a bold
  label. A row that closes or deletes something is red, and a brighter red
  when selected. A row that cannot be chosen yet is grey. A row's key, when
  it has one, sits in a key cap at the row's right end and runs it. In every
  list, Up and Down move the selection, Enter runs the selected row and
  Escape closes the overlay. The palette and the settings panel now take the
  pointer as the right click menu does. The pointer selects the row under
  it, and a click runs that row. The palette's search line starts with an
  orange ›. While it is empty it shows "type to search", after the submenu's
  name when one is open. The settings panel opens in the middle of the
  window with a title and a line saying that Left and Right change the
  value. A click on a value's ‹ or › changes it. The name field is a small
  box titled for what it renames, such as Rename pane, with the palette's
  search line. The reason for a refused name shows in a red row under the
  line. Key hints for Enter and Escape end the box.
- The picker frames each workspace in a rounded outline. The selected
  workspace's outline is two strokes thick. Each label starts with the mark
  of its workspace's agent state. The current workspace's name is bold.
  Labels are cut to their cell. After one press on a workspace's x, its
  frame and label turn red and the label says Enter closes it. The x is
  larger.
- An open palette, settings panel or picker closes when the active viewer
  disconnects.
- Long names are cut with an ellipsis on the Ctrl bar, in the top bar and
  in the palette. decade draws no tab button in the top bar where it would
  reach the agent counts.
- The New workspace browser shows "no folders" when nothing matches.
- Settings values line up in one column. "Bar hold" is now "Ctrl bar
  delay" and "Grab animation" is now "Drag pickup time".
- A pane whose shell exited keeps its normal background and shows a larger
  restart button.
- The Ctrl bar's icons share one size. The pin is filled when pinned and
  outlined when not.
- The gaps between panes match the margin at the edge of the pane area.
  Unfocused outlines are brighter. A pinned pane shows a pin on its top
  edge, and a zoomed pane shows a zoom tag there. Each tag is a small
  rounded pill in the outline's colour.

## [0.1.0] - 2026-11-05

The name on crates.io, and a window.

### Added

- A wgpu window that lays out three placeholder panes and a status line on a
  grid of 9 by 18 pixel cells. The panes draw nothing yet.

[0.1.0]: https://github.com/jackroddy/decade/releases/tag/v0.1.0
