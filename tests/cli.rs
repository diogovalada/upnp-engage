use std::{
    fs,
    process::{Command, Stdio},
};

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_upnp-engage"))
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn help_and_version_do_not_need_config_or_router() {
    for flag in ["--help", "--version"] {
        let output = run(&[flag]);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("upnp-engage"));
    }
}

#[test]
fn missing_settings_fail_without_prompting_or_creating_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.toml");
    let output = run(&["--config", path.to_str().unwrap(), "--non-interactive"]);
    assert!(!output.status.success());
    assert!(!path.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("device port is missing"));
}

#[test]
fn invalid_file_is_preserved_even_with_overrides_and_save() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.toml");
    fs::write(&path, "do not erase [").unwrap();
    let output = run(&[
        "--config",
        path.to_str().unwrap(),
        "--device-port",
        "8080",
        "--save-config",
        "--non-interactive",
    ]);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(path).unwrap(), "do not erase [");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid config"));
}

#[test]
fn explicit_interactive_mode_rejects_missing_terminal() {
    let output = run(&["--interactive"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs a terminal"));
}
