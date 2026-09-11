//! The simple case: the servers are running in this process.
//!
//! An editor on files this machine holds needs no HTTP and no protocol of its own - it needs
//! a [`moon_lsp::LspRegistry`] and a repo root. This is that, written down once so nobody has
//! to write it again, and it is what makes the crate usable on its own rather than a set of
//! parts waiting for an application to assemble them.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use moon_lsp::{
    LspCompletion, LspFileEdit, LspLocation, LspPosition, LspRegistry, LspStatus, Workspace,
};

use crate::source::LanguageSource;

/// Language servers running here, for one repo.
///
/// The registry is shared rather than owned: one set of servers serves every file open on a
/// repo, and starting rust-analyzer once per tab would index the project once per tab. Build
/// one registry for the application, and one of these per repo it has open.
pub struct RegistrySource {
    servers: Arc<LspRegistry>,
    /// What this repo's servers are held under. See [`Workspace::key`] - the registry never
    /// looks inside it, so it can be the repo path, one window's work, or anything else the
    /// caller wants a set of servers to live and die with.
    key: String,
    root: PathBuf,
}

impl RegistrySource {
    /// Servers for the repo at `root`, held in `servers` under `key`.
    pub fn new(
        servers: Arc<LspRegistry>,
        key: impl Into<String>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            servers,
            key: key.into(),
            root: root.into(),
        }
    }

    /// Servers for the repo at `root`, held under the repo's own path.
    ///
    /// The one-line version, for an application with a single repo open: there is nothing to
    /// share a key between, so the path is as good a key as any.
    pub fn for_repo(servers: Arc<LspRegistry>, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self::new(servers, root.to_string_lossy().into_owned(), root)
    }

    fn repo(&self) -> Workspace<'_> {
        Workspace {
            key: &self.key,
            root: &self.root,
        }
    }
}

impl LanguageSource for RegistrySource {
    fn status(&self, file_path: &str) -> LspStatus {
        self.servers.status(&self.key, file_path)
    }

    fn did_open(&self, file_path: &str, text: &str) -> Result<()> {
        self.servers.did_open(&self.repo(), file_path, text)
    }

    fn did_change(&self, file_path: &str, text: &str) -> Result<()> {
        self.servers.did_change(&self.repo(), file_path, text)
    }

    fn did_close(&self, file_path: &str) -> Result<()> {
        self.servers.did_close(&self.repo(), file_path)
    }

    fn places(
        &self,
        file_path: &str,
        at: LspPosition,
        which: moon_lsp::LspPlaces,
    ) -> Result<Vec<LspLocation>> {
        self.servers.places(&self.repo(), file_path, at, which)
    }

    fn format(
        &self,
        file_path: &str,
        options: moon_lsp::LspFormatting,
    ) -> Result<Vec<moon_lsp::LspTextEdit>> {
        self.servers.format(&self.repo(), file_path, options)
    }

    fn hover(&self, file_path: &str, at: LspPosition) -> Result<Option<String>> {
        self.servers.hover(&self.repo(), file_path, at)
    }

    fn diagnostics(&self, file_path: &str) -> Result<Vec<moon_lsp::LspDiagnostic>> {
        Ok(self.servers.diagnostics(&self.repo(), file_path))
    }

    fn did_save(&self, file_path: &str) -> Result<()> {
        self.servers.did_save(&self.repo(), file_path)
    }

    fn code_actions(
        &self,
        file_path: &str,
        at: LspPosition,
    ) -> Result<Vec<moon_lsp::LspCodeAction>> {
        self.servers.code_actions(&self.repo(), file_path, at)
    }

    fn signature_help(
        &self,
        file_path: &str,
        at: LspPosition,
    ) -> Result<Option<moon_lsp::LspSignature>> {
        self.servers.signature_help(&self.repo(), file_path, at)
    }

    fn completion(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCompletion>> {
        self.servers.completion(&self.repo(), file_path, at)
    }

    fn trigger_characters(&self, file_path: &str) -> Vec<char> {
        self.servers.trigger_characters(&self.key, file_path)
    }

    fn prepare_rename(&self, file_path: &str, at: LspPosition) -> Result<Option<String>> {
        self.servers.prepare_rename(&self.repo(), file_path, at)
    }

    fn rename(&self, file_path: &str, at: LspPosition, new_name: &str) -> Result<Vec<LspFileEdit>> {
        self.servers.rename(&self.repo(), file_path, at, new_name)
    }
}
