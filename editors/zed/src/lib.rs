use std::fs;
use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, Architecture, DownloadedFileType, GithubReleaseOptions,
    LanguageServerId, LanguageServerInstallationStatus, Os, Result,
};

/// GitHub `owner/name` the server is downloaded from (asset names come from
/// .github/workflows/release.yml). The only place the repository is named in this file.
const REPOSITORY: &str = "gsql-lsp/gsql-lsp";

struct GsqlExtension {
    /// Path of the downloaded binary, relative to the extension's working directory.
    cached_binary_path: Option<String>,
}

impl GsqlExtension {
    /// The release target triple, archive type and extension for the current platform.
    fn release_target() -> Result<(&'static str, DownloadedFileType, &'static str)> {
        let (os, arch) = zed::current_platform();
        match (os, arch) {
            (Os::Linux, Architecture::X8664) => Ok(("x86_64-unknown-linux-musl", DownloadedFileType::GzipTar, "tar.gz")),
            (Os::Linux, Architecture::Aarch64) => {
                Ok(("aarch64-unknown-linux-musl", DownloadedFileType::GzipTar, "tar.gz"))
            }
            (Os::Mac, Architecture::X8664) => Ok(("x86_64-apple-darwin", DownloadedFileType::GzipTar, "tar.gz")),
            (Os::Mac, Architecture::Aarch64) => Ok(("aarch64-apple-darwin", DownloadedFileType::GzipTar, "tar.gz")),
            (Os::Windows, Architecture::X8664) => Ok(("x86_64-pc-windows-msvc", DownloadedFileType::Zip, "zip")),
            _ => Err(
                "no gsql-lsp release is built for this platform; install it with `cargo install --path crates/gsql-lsp`"
                    .to_string(),
            ),
        }
    }

    /// The binary of the newest `gsql-lsp-<version>/` directory on disk that has one.
    fn downloaded_binary(target: &str, extension: &str) -> Option<String> {
        let exe = if extension == "zip" { "gsql-lsp.exe" } else { "gsql-lsp" };
        let mut found: Vec<String> = fs::read_dir(".")
            .ok()?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("gsql-lsp-"))
            .map(|name| format!("{name}/gsql-lsp-{target}/{exe}"))
            .filter(|path| fs::metadata(path).is_ok_and(|m| m.is_file()))
            .collect();
        found.sort();
        found.pop()
    }

    /// Downloads the latest release (once per version) and returns the binary path.
    fn download_binary(&mut self, id: &LanguageServerId) -> Result<String> {
        // A binary that is still on disk is reused without asking GitHub again.
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
        }

        let (target, file_type, extension) = Self::release_target()?;
        zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::CheckingForUpdate);
        let release = zed::latest_github_release(
            REPOSITORY,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        );
        let release = match release {
            Ok(release) => release,
            Err(e) => {
                // Offline: an older download that is still on disk beats no server.
                if let Some(path) = Self::downloaded_binary(target, extension) {
                    zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::None);
                    self.cached_binary_path = Some(path.clone());
                    return Ok(path);
                }
                let message = format!("could not look up the latest gsql-lsp release: {e}");
                zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::Failed(message.clone()));
                return Err(message);
            }
        };

        let asset_name = format!("gsql-lsp-{target}.{extension}");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("release {} has no asset {asset_name}", release.version))?;

        // The archive holds a `gsql-lsp-<target>/` folder with the binary.
        let version_dir = format!("gsql-lsp-{}", release.version);
        let exe = if extension == "zip" { "gsql-lsp.exe" } else { "gsql-lsp" };
        let binary_path = format!("{version_dir}/gsql-lsp-{target}/{exe}");

        if !fs::metadata(&binary_path).is_ok_and(|m| m.is_file()) {
            zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::Downloading);
            let result = zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|e| format!("could not download {asset_name}: {e}"))
                .and_then(|()| {
                    zed::make_file_executable(&binary_path)
                        .map_err(|e| format!("could not make gsql-lsp executable: {e}"))
                });
            if let Err(message) = result {
                zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::Failed(message.clone()));
                return Err(message);
            }

            // Remove the directories of older versions.
            if let Ok(entries) = fs::read_dir(".") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("gsql-lsp-") && name != version_dir {
                        fs::remove_dir_all(entry.path()).ok();
                    }
                }
            }
        }

        zed::set_language_server_installation_status(id, &LanguageServerInstallationStatus::None);
        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for GsqlExtension {
    fn new() -> Self {
        GsqlExtension {
            cached_binary_path: None,
        }
    }

    /// Order: `lsp.gsql-lsp.binary.path` (handled by Zed before this is called), `gsql-lsp`
    /// on the worktree's PATH, then a release download.
    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let command = match worktree.which("gsql-lsp") {
            Some(path) => path,
            None => self.download_binary(language_server_id)?,
        };
        Ok(zed::Command {
            command,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        Ok(LspSettings::for_worktree("gsql-lsp", worktree).ok().and_then(|settings| settings.initialization_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        Ok(LspSettings::for_worktree("gsql-lsp", worktree).ok().and_then(|settings| settings.settings))
    }
}

zed::register_extension!(GsqlExtension);
