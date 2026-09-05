//! A file open in a window, with a real language server behind it:
//! `cargo run --example edit -- src/lib.rs`.
//!
//! Type, and a list of what the server would finish the word with appears under the caret -
//! arrows to move, Enter to take, Escape to put away. ⌘-click (ctrl-click off macOS) a name
//! and the window opens the file it is defined in, scrolled to the line - including into a
//! dependency or the standard library, which for rust is where most names live. `⌘S` writes
//! the file back, and the button along the top goes back to where the jump came from.
//!
//! The path is read relative to the current directory, which is also the repo the servers are
//! started in - so run it from the root of the project you want answered about.

use std::sync::Arc;

use egui_moon_code_ide::{CodeEditor, Definition, RegistrySource};
use egui_moon_editor::{EditorRequest, EditorStyle};

/// Where a jump came from, so there is a way back out of a name that turned out to be the
/// wrong one. The file is named the way the source names it, and the line is the one the
/// click was on.
struct CameFrom {
    file_path: String,
    line: usize,
}

struct EditWindow {
    /// The repo the servers are reading, and what a file's name is resolved against.
    repo: std::path::PathBuf,
    /// The file on screen, as the source names it: relative to the repo for a file inside it,
    /// absolute for one outside.
    file_path: String,
    code: CodeEditor,
    status: String,
    /// The line a jump asked to be shown, until the widget has laid it out once. Cleared then
    /// and not before: asking every frame would drag the view back to it and the file could
    /// not be scrolled away from.
    showing_line: Option<usize>,
    came_from: Option<CameFrom>,
}

impl EditWindow {
    /// The file on disk, whatever its name means.
    ///
    /// `LspLocation::file_path` is relative to the repo for a file inside it and absolute for
    /// one outside - a dependency's source, or the standard library. `Path::join` is both
    /// rules at once, since an absolute path replaces the root rather than hanging off it,
    /// and it is the same join `moon_lsp` does to turn the name back into a document.
    fn on_disk(&self, file_path: &str) -> std::path::PathBuf {
        self.repo.join(file_path)
    }

    /// Show `file_path` at `line`, which is what following a definition comes to.
    ///
    /// A file that cannot be read is said so rather than opened: an empty buffer would look
    /// like a file with nothing in it, and typing into it and saving would write that.
    fn jump_to(&mut self, file_path: String, line: usize, from: CameFrom) {
        let text = match std::fs::read_to_string(self.on_disk(&file_path)) {
            Ok(text) => text,
            Err(error) => {
                self.status = format!("{file_path}: {error}");
                return;
            }
        };
        // The editor keeps its worker thread and tells the server the old file is closed and
        // this one is open - see `CodeEditor::open`.
        self.code.open(&file_path, text);
        self.file_path = file_path;
        self.showing_line = Some(line);
        self.came_from = Some(from);
        self.status = String::new();
    }
}

impl eframe::App for EditWindow {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            let save =
                ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
            if save {
                let path = self.on_disk(&self.file_path);
                self.status = match std::fs::write(&path, self.code.text()) {
                    Ok(()) => format!("wrote {}", path.display()),
                    Err(error) => format!("{error}"),
                };
            }
            let mut back = None;
            ui.horizontal(|ui| {
                ui.label(&self.file_path);
                ui.label(format!("{:?}", self.code.status()).to_lowercase());
                if let Some(from) = &self.came_from
                    && ui.button(format!("back to {}", from.file_path)).clicked()
                {
                    back = Some((from.file_path.clone(), from.line));
                }
                ui.label(&self.status);
            });
            if let Some((file_path, line)) = back {
                let here = CameFrom {
                    file_path: self.file_path.clone(),
                    line: self.showing_line.unwrap_or(1),
                };
                self.jump_to(file_path, line, here);
                self.came_from = None;
            }

            let style = EditorStyle::from_visuals(ui.visuals());
            let output = self.code.ui(
                ui,
                &style,
                &EditorRequest {
                    line_of_interest: self.showing_line,
                    ..EditorRequest::default()
                },
            );
            // Once the line has been laid out the window is done asking for it. Anything else
            // would pin the view there and the file could not be scrolled.
            if output.editor.line_at.is_some() {
                self.showing_line = None;
            }

            // Where a ⌘-click landed.
            match output.definition {
                Some(Definition::Places { word, places }) => match places.first() {
                    Some(place) => {
                        let here = CameFrom {
                            file_path: self.file_path.clone(),
                            // The protocol counts lines from zero and the widget from one.
                            line: word.at.line + 1,
                        };
                        self.jump_to(place.file_path.clone(), place.line_number, here);
                    }
                    None => self.status = format!("the server has no definition for {}", word.text),
                },
                // rust-analyzer indexes for tens of seconds, and answers every question with
                // nothing while it does. Saying so is the difference between a wait and a bug.
                Some(Definition::StillStarting(waiting)) => self.status = waiting,
                Some(Definition::NoServer(word)) => {
                    self.status = format!(
                        "no language server serves this file, so {} is unanswered",
                        word.text
                    );
                }
                None => {}
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let file_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "src/lib.rs".to_string());
    let repo = std::env::current_dir().expect("a directory to start the servers in");
    let text = std::fs::read_to_string(repo.join(&file_path)).unwrap_or_default();
    // The login shell's PATH rather than this process's: a window started from a desktop
    // launcher inherits neither homebrew nor `~/.local/bin`, and that is where the servers are.
    let servers = Arc::new(moon_lsp::LspRegistry::new(
        std::env::var("PATH").unwrap_or_default(),
    ));

    eframe::run_native(
        "egui_moon_code_ide",
        eframe::NativeOptions::default(),
        Box::new(move |cc| {
            let source = RegistrySource::for_repo(servers, &repo);
            let code = CodeEditor::new(&cc.egui_ctx, &file_path, text, Arc::new(source));
            Ok(Box::new(EditWindow {
                repo,
                file_path,
                code,
                status: String::new(),
                showing_line: None,
                came_from: None,
            }))
        }),
    )
}
