# Changelog

## Unreleased

- `LanguageSource::code_actions` and `signature_help`, and `Signing`: when the call being typed
  is worth asking about - the caret settled inside an unclosed `(`, on text the server has
  heard - and which answer is still the one to show.

- `LanguageSource::hover`, `diagnostics` and `did_save`, and `Hovering`: when resting the
  pointer on a name is worth a question - once it has rested, only about text the server has
  heard, and once per word.

- `LanguageSource::places` and `LanguageSource::format`. `definition` is now `places` asked for
  definitions, so a source implements the one and gets the other.

- `LanguageSource` asks two more questions, `prepare_rename` and `rename`, and `RegistrySource`
  answers them out of the registry. They are required rather than given a default: a source
  that cannot rename would have to answer with nothing, which reads as a name used nowhere.

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
