use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub device_port: u16,
    /// Zero deliberately retains the "same as device" preference.
    pub router_port: u16,
}

impl Config {
    pub fn external_port(self) -> u16 {
        if self.router_port == 0 {
            self.device_port
        } else {
            self.router_port
        }
    }
    pub fn validate(self) -> Result<Self> {
        if self.device_port == 0 {
            bail!("A device port from 1 to 65535 is required.");
        }
        Ok(self)
    }
    pub fn overlay(mut self, device: Option<u16>, router: Option<u16>) -> Self {
        if let Some(port) = device {
            self.device_port = port;
        }
        if let Some(port) = router {
            self.router_port = port;
        }
        self
    }
}

pub struct ConfigFile {
    pub path: PathBuf,
    pub config: Config,
    pub issue: Option<String>,
    pub explicit: bool,
    original: Option<Vec<u8>>,
    document: Option<DocumentMut>,
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse(bytes: &[u8]) -> Result<(DocumentMut, Config)> {
    let document = std::str::from_utf8(bytes)?.parse::<DocumentMut>()?;
    let port = |name: &str| -> Result<u16> {
        match document.get(name) {
            None => Ok(0),
            Some(item) => item
                .as_integer()
                .and_then(|number| u16::try_from(number).ok())
                .with_context(|| format!("{name} must be a whole number from 0 to 65535")),
        }
    };
    let config = Config {
        device_port: port("device_port")?,
        router_port: port("router_port")?,
    };
    Ok((document, config))
}

impl ConfigFile {
    pub fn open(path: PathBuf, explicit: bool) -> Result<Self> {
        let original =
            read_optional(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        let mut file = Self {
            path,
            explicit,
            original,
            config: Config::default(),
            issue: None,
            document: None,
        };
        if let Some(bytes) = &file.original {
            match parse(bytes) {
                Ok((document, config)) => {
                    file.document = Some(document);
                    file.config = config;
                }
                Err(error) => {
                    file.issue = Some(format!(
                        "Invalid config at {}: {error}",
                        file.path.display()
                    ))
                }
            }
        }
        Ok(file)
    }
    pub fn exists(&self) -> bool {
        self.original.is_some()
    }

    pub fn save(&mut self, config: Config, replace_invalid: bool) -> Result<()> {
        config.validate()?;
        if self.issue.is_some() && !replace_invalid {
            bail!(
                "The config is invalid. Choose to replace it interactively or fix the file first."
            );
        }
        self.check_unchanged()?;
        let mut document = self.document.clone().unwrap_or_else(|| {
            "device_port = 0\n# 0 uses the device port.\nrouter_port = 0\n"
                .parse()
                .expect("valid built-in config template")
        });
        for (key, number) in [
            ("device_port", config.device_port),
            ("router_port", config.router_port),
        ] {
            let decor = document
                .get(key)
                .and_then(|item| item.as_value())
                .map(|v| v.decor().clone());
            document[key] = value(i64::from(number));
            if let Some(decor) = decor {
                *document[key].as_value_mut().unwrap().decor_mut() = decor;
            }
        }
        let contents = document.to_string();
        let parent = self
            .path
            .parent()
            .context("Config has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        if let Ok(metadata) = fs::metadata(&self.path) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.write_all(contents.as_bytes())?;
        temporary.as_file().sync_all()?;
        self.check_unchanged()?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("Cannot save {}", self.path.display()))?;
        self.original = Some(contents.into_bytes());
        self.document = Some(document);
        self.config = config;
        self.issue = None;
        Ok(())
    }

    fn check_unchanged(&self) -> Result<()> {
        if read_optional(&self.path)? != self.original {
            bail!(
                "{} changed since it was loaded. Reload it before saving.",
                self.path.display()
            );
        }
        Ok(())
    }
}

pub fn user_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("upnp-engage").join("config.toml"))
}

pub fn discover(
    explicit: Option<&Path>,
    executable: &Path,
    cwd: &Path,
    user: Option<&Path>,
) -> Result<ConfigFile> {
    if let Some(path) = explicit {
        return ConfigFile::open(
            if path.is_absolute() {
                path.to_owned()
            } else {
                cwd.join(path)
            },
            true,
        );
    }
    let directory = executable
        .parent()
        .context("Cannot find executable directory")?;
    let portable = directory.join("config.toml");
    let mut candidates = vec![portable.clone(), cwd.join("config.toml")];
    if let Some(user) = user {
        candidates.push(user.to_owned());
    }
    candidates.dedup();
    for candidate in candidates {
        let file = ConfigFile::open(candidate, false)?;
        if file.exists() {
            return Ok(file);
        }
    }
    let installed = ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .iter()
        .any(|p| directory == Path::new(p))
        || env::var_os("HOME")
            .is_some_and(|home| directory == PathBuf::from(home).join(".local/bin"));
    let destination = if installed {
        user.unwrap_or(&portable)
    } else {
        &portable
    };
    ConfigFile::open(destination.to_owned(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_follow_device_overrides_but_explicit_ports_do_not() {
        let (_, config) = parse(b"device_port = 80\n").unwrap();
        assert_eq!(config.overlay(Some(8080), None).external_port(), 8080);
        assert_eq!(config.overlay(Some(8080), Some(9000)).external_port(), 9000);
        assert!(Config::default().validate().is_err());
        for invalid in ["-1", "65536", "1.5", "\"80\""] {
            assert!(parse(format!("device_port = {invalid}").as_bytes()).is_err());
        }
    }
    #[test]
    fn discovery_selects_one_file_and_preserves_invalid_contents() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("portable");
        let cwd = root.path().join("working");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(exe_dir.join("config.toml"), "broken [").unwrap();
        fs::write(cwd.join("config.toml"), "device_port = 8080").unwrap();
        let mut file = discover(None, &exe_dir.join("program"), &cwd, None).unwrap();
        assert!(file.issue.is_some());
        assert!(file
            .save(
                Config {
                    device_port: 80,
                    router_port: 0
                },
                false
            )
            .is_err());
        assert_eq!(fs::read_to_string(file.path).unwrap(), "broken [");
        let explicit = discover(
            Some(Path::new("other.toml")),
            &exe_dir.join("program"),
            &cwd,
            None,
        )
        .unwrap();
        assert!(!explicit.exists());
        assert_eq!(explicit.path, cwd.join("other.toml"));
    }
    #[test]
    fn save_preserves_comments_and_rejects_external_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        fs::write(
            &path,
            "# my server\ndevice_port = 80 # app\nrouter_port = 0\nother = 'keep'\n",
        )
        .unwrap();
        let mut file = ConfigFile::open(path.clone(), false).unwrap();
        file.save(
            Config {
                device_port: 8080,
                router_port: 0,
            },
            false,
        )
        .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# my server"));
        assert!(saved.contains("# app"));
        assert!(saved.contains("other = 'keep'"));
        fs::write(&path, "device_port = 1234").unwrap();
        assert!(file
            .save(
                Config {
                    device_port: 9000,
                    router_port: 0
                },
                false
            )
            .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "device_port = 1234");
    }
    #[test]
    fn missing_and_failed_saves_do_not_create_placeholders() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let mut file = ConfigFile::open(path.clone(), false).unwrap();
        assert!(!path.exists());
        assert!(file.save(Config::default(), false).is_err());
        assert!(!path.exists());
        fs::write(&path, "someone else wrote this").unwrap();
        assert!(file
            .save(
                Config {
                    device_port: 80,
                    router_port: 0
                },
                false
            )
            .is_err());
    }

    #[test]
    fn newly_saved_config_round_trips_with_comments_and_both_ports() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let mut file = ConfigFile::open(path.clone(), false).unwrap();
        let settings = Config {
            device_port: 8081,
            router_port: 9001,
        };
        file.save(settings, false).unwrap();
        let loaded = ConfigFile::open(path, false).unwrap();
        assert!(loaded.issue.is_none());
        assert_eq!(loaded.config, settings);
    }
}
