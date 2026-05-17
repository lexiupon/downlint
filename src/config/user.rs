use std::path::PathBuf;

pub fn user_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            dirs_home()
                .join("Library")
                .join("Application Support")
                .join("downlint")
                .join("config.toml"),
        )
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("downlint").join("config.toml"))
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(path).join("downlint").join("config.toml"))
        } else {
            Some(
                dirs_home()
                    .join(".config")
                    .join("downlint")
                    .join("config.toml"),
            )
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
