# Changelog

## 0.1.0

First release. The seam between `egui_moon_editor` and `moon_lsp`: `LanguageSource` is the six
questions an editor puts to a language server, written so they can be answered from another
machine; `RegistrySource` answers them out of an `LspRegistry` running here; `Served` says what
a server is owed about an open buffer and debounces the change; `Completing` decides when a
half-typed word is worth a question and throws away an answer the buffer has moved out from
under; `asks_about` and `still_starting` are what a modifier-click can expect of a server that
is still indexing; `Asking` is the worker thread the blocking calls go on; and `CodeEditor` is
the whole of it wired together as one widget - including `CodeEditor::open`, which follows a
jump into another file on the thread it already has.
