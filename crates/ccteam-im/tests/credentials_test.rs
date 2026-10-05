//! Credentials reader integration tests.

use ccteam_im::credentials::{load, save, Credentials, LarkCreds, SlackCreds, TelegramCreds};
use tempfile::TempDir;

#[test]
fn missing_file_returns_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("none.json");
    let c = load(Some(&path)).unwrap();
    assert!(c.telegram.is_none());
    assert!(c.slack.is_none());
    assert!(c.discord.is_none());
    assert!(c.lark.is_none());
}

#[test]
fn round_trip_all_platforms() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("c.json");
    let original = Credentials {
        telegram: Some(TelegramCreds {
            bot_token: "TG:abc".into(),
            allowed_chat_ids: vec!["1".into(), "2".into()],
            require_mention: false,
        }),
        slack: Some(SlackCreds {
            bot_token: "xoxb-x".into(),
            app_token: "xapp-x".into(),
            allowed_user_ids: vec!["U123".into()],
            require_mention: false,
        }),
        discord: None,
        lark: Some(LarkCreds {
            app_id: "cli_x".into(),
            app_secret: "sek".into(),
            allowed_user_ids: vec!["ou_a".into()],
            use_feishu: true,
            require_mention: false,
        }),
    };
    save(&path, &original).unwrap();
    let back = load(Some(&path)).unwrap();
    assert_eq!(back, original);
}

/// `require_mention` is the same switch on every IM and defaults to OFF
/// (answer everything) when the key is absent — an existing credentials file
/// keeps behaving as it did — and survives a save/load when on.
#[test]
fn require_mention_defaults_off_and_round_trips_on_every_im() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("c.json");
    std::fs::write(
        &path,
        r#"{"telegram":{"bot_token":"t"},
            "lark":{"app_id":"a","app_secret":"s"},
            "slack":{"bot_token":"b","app_token":"x"}}"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&path).unwrap().permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&path, p).unwrap();
    }
    let mut c = load(Some(&path)).unwrap();
    assert!(!c.telegram.as_ref().unwrap().require_mention);
    assert!(!c.lark.as_ref().unwrap().require_mention);
    assert!(!c.slack.as_ref().unwrap().require_mention);

    c.telegram.as_mut().unwrap().require_mention = true;
    c.lark.as_mut().unwrap().require_mention = true;
    c.slack.as_mut().unwrap().require_mention = true;
    save(&path, &c).unwrap();
    let back = load(Some(&path)).unwrap();
    assert!(back.telegram.unwrap().require_mention);
    assert!(back.lark.unwrap().require_mention);
    assert!(back.slack.unwrap().require_mention);
}

#[test]
fn empty_object_parses_to_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("c.json");
    std::fs::write(&path, "{}").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&path).unwrap().permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&path, p).unwrap();
    }
    let c = load(Some(&path)).unwrap();
    assert_eq!(c, Credentials::default());
}
