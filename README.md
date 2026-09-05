# egui_moon_code_ide

An [egui](https://github.com/emilk/egui) code editor with go-to-definition and autocomplete.

There is a widget — [`egui_moon_editor`](../egui_moon_editor) — which owns a buffer, draws it,
draws a completion list it is *handed*, and reports the word under a modifier-click. And there
is a client — [`moon_lsp`](../moon_lsp) — which starts language servers, carries the JSON-RPC
and does the position arithmetic. Neither knows the other exists, and that is right: a widget
with a subprocess in it is not a widget.

What was missing is everything between them, which turns out to be most of the work:

- when to tell a server the text has changed, and when not to — a round trip per keystroke
  floods a server, and over a network floods the link
- when a half-typed word is worth asking about, and when the answer that comes back is for text
  that no longer exists and has to be thrown away
- telling "the server is still indexing" apart from "there is no definition", which look
  identical on the wire and could not be more different to the person waiting
- which rows of a server's answer are actually offered: the protocol leaves the filtering to
  whoever asked, and a bare position answers with everything in scope

```rust
use std::sync::Arc;
use egui_moon_code_ide::{CodeEditor, RegistrySource};

let servers = Arc::new(moon_lsp::LspRegistry::new(std::env::var("PATH")?));
let source = RegistrySource::for_repo(servers, "/home/dev/repo");
let mut code = CodeEditor::new(ctx, "src/main.rs", text, Arc::new(source));

// and, each frame:
let style = egui_moon_editor::EditorStyle::from_visuals(ui.visuals());
let output = code.ui(ui, &style, &egui_moon_editor::EditorRequest::default());
```

A whole file open in a window, against a real language server:

```sh
cargo run --example edit -- src/lib.rs
```

Type in it and a list of what the server would finish the word with appears under the caret;
⌘-click a name and the window opens the file it is defined in, scrolled to the line - a
dependency or the standard library included; and while rust-analyzer is still reading the
project it says so rather than shrugging.

## The seam

The answers do not have to come from this process. `LanguageSource` is six questions and
nothing else — status, open, change, close, definition, completion — because a window reviewing
a repo on another machine reaches its servers over HTTP: the repo is over there, and so is
anything that could read it. An editor built on this crate cannot tell the difference, and
should not be able to. `RegistrySource` answers those six out of a `moon_lsp::LspRegistry`
running here, so the simple case is two lines rather than homework.

Everything above the trait stays with the caller. Where a jump lands, what to do when there is
no server — a repo search, a tags file, nothing — what a status bar says, and whether two files
share a set of servers: none of that is this crate's business, and all of it is a product's.

## Threading

Every one of the six blocks, sometimes for tens of seconds, and an egui application must never
wait on a frame. So `CodeEditor` owns a worker thread, puts its questions on it, and reads the
answers back on whatever later frame they land. Questions waiting to go out sit in a slot per
kind rather than a queue, so a newer one replaces the one it supersedes instead of piling up
behind it.

An editor that follows a jump takes that thread with it - `CodeEditor::open` closes the old
document on the server, opens the new one, and drops whatever was in flight about the file
left behind. A `CodeEditor` per click would be a thread per click, and a server left believing
every file ever visited is still open.

An application that already has worker threads of its own can skip all of that and drive
`Served`, `Completing` and `asks_about` from them: they hold no channels, take the clock as an
argument, and are where all of the deciding lives.

## License

MIT
