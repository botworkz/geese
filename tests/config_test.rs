#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../src/error.rs"]
mod error;
#[allow(dead_code)]
#[path = "../src/paths.rs"]
mod paths;

use config::Config;

#[test]
fn yaml_round_trip_preserves_values() {
    let yaml = r#"
defaults:
  binary: goose
  args: ["--default"]
  env:
    SHARED: yes
profiles:
  work:
    args: ["--profile"]
    env:
      TOKEN: abc
"#;

    let config = Config::from_yaml_str(yaml).expect("config should parse");
    let emitted = serde_yaml::to_string(&config).expect("config should serialize");
    let reparsed = Config::from_yaml_str(&emitted).expect("config should round-trip");

    assert_eq!(config, reparsed);
}

#[test]
fn validation_rejects_invalid_profile_names() {
    let yaml = r#"
profiles:
  "My Profile!": {}
"#;

    let error = Config::from_yaml_str(yaml).expect_err("invalid names should fail");
    assert!(error
        .to_string()
        .contains("invalid profile name 'My Profile!': expected ^[a-z0-9][a-z0-9_-]*$"));
}

#[test]
fn validation_rejects_empty_profiles() {
    let yaml = r#"
profiles: {}
"#;

    let error = Config::from_yaml_str(yaml).expect_err("empty profiles should fail");
    assert!(error
        .to_string()
        .contains("config must define at least one profile in 'profiles'"));
}

#[test]
fn example_config_parses_with_loader() {
    let path = std::path::Path::new("config.example.yml");
    let config = Config::from_path(path).expect("example config should parse");

    assert!(config.profiles.contains_key("work"));
    assert!(config.profiles.contains_key("personal"));
    assert!(config.profiles.contains_key("scratch"));
}
