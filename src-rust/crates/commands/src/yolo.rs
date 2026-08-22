// Yolo command: switch the permission mode between asking and never asking
// (`/yolo`).
//
// There is no separate `yoloMode` setting. "Yolo" is a name for
// `permissionMode: "bypassPermissions"`, which already exists, is already
// documented, and is already what `--dangerously-skip-permissions` (visible
// alias `--yolo`) sets. A second key would let a settings file say two
// contradictory things about the same state.

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;
use mikmik_core::config::PermissionMode;

pub struct YoloCommand;

/// What `/yolo <args>` asks for.
#[derive(Debug, PartialEq, Eq)]
enum YoloRequest {
    /// No argument: switch it the other way.
    Toggle,
    /// Stop asking for permission.
    On,
    /// Go back to asking.
    Off,
    /// Report the mode in force without changing it.
    Show,
    /// The argument made no sense; the string names what was wrong.
    Invalid(String),
}

fn parse_request(args: &str) -> YoloRequest {
    let args = args.trim();
    if args.is_empty() {
        return YoloRequest::Toggle;
    }
    if args.eq_ignore_ascii_case("status") {
        return YoloRequest::Show;
    }
    if args.eq_ignore_ascii_case("on") || args.eq_ignore_ascii_case("enable") {
        return YoloRequest::On;
    }
    if args.eq_ignore_ascii_case("off") || args.eq_ignore_ascii_case("disable") {
        return YoloRequest::Off;
    }
    YoloRequest::Invalid(args.to_string())
}

/// The mode `request` leaves the session in, given the one it is in now.
///
/// Switching off returns to `Default` rather than to whatever mode preceded
/// bypass, because nothing records that and guessing `acceptEdits` would hand
/// back more than was taken away.
fn next_mode(request: &YoloRequest, current: PermissionMode) -> PermissionMode {
    match request {
        YoloRequest::On => PermissionMode::BypassPermissions,
        YoloRequest::Off => PermissionMode::Default,
        YoloRequest::Toggle => match current {
            PermissionMode::BypassPermissions => PermissionMode::Default,
            _ => PermissionMode::BypassPermissions,
        },
        YoloRequest::Show | YoloRequest::Invalid(_) => current,
    }
}

/// How the mode in force reads.
fn describe(mode: PermissionMode) -> String {
    match mode {
        PermissionMode::BypassPermissions => {
            "Yolo mode is ON. Every tool runs without asking, including ones that \
             write files and run shell commands."
                .to_string()
        }
        PermissionMode::Default => "Yolo mode is off: tools ask before acting.".to_string(),
        PermissionMode::AcceptEdits => {
            "Yolo mode is off. The session is in accept-edits mode: file edits go \
             through without asking, everything else still asks."
                .to_string()
        }
        PermissionMode::Plan => {
            "Yolo mode is off. The session is in plan mode, where nothing acts at all.".to_string()
        }
    }
}

#[async_trait]
impl SlashCommand for YoloCommand {
    fn name(&self) -> &str {
        "yolo"
    }

    fn description(&self) -> &str {
        "Run every tool without asking for permission"
    }

    fn help(&self) -> &str {
        "Usage:\n\
         /yolo            — switch it the other way\n\
         /yolo on         — stop asking for permission\n\
         /yolo off        — go back to asking\n\
         /yolo status     — show the mode in force\n\n\
         Yolo mode is `permissionMode: \"bypassPermissions\"` under a shorter\n\
         name. Every tool runs unasked, including ones that write files and\n\
         run shell commands, so nothing stands between the model and your\n\
         working tree. The mode is saved in settings and survives a restart;\n\
         Shift+Tab cycles it for one session only."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let request = parse_request(args);
        match request {
            YoloRequest::Show => {
                return CommandResult::Message(describe(ctx.config.permission_mode))
            }
            YoloRequest::Invalid(arg) => {
                return CommandResult::Error(format!(
                    "{:?} is not a yolo setting. Use `on`, `off`, or `status`.",
                    arg
                ));
            }
            _ => {}
        }

        let mode = next_mode(&request, ctx.config.permission_mode);

        // The file takes the change before the session does, so a failed write
        // cannot report a mode it saved nowhere.
        if let Err(e) = super::save_settings_mutation(|s| s.config.permission_mode = mode) {
            return CommandResult::Error(format!("Could not save the permission mode: {}", e));
        }

        let mut config = ctx.config.clone();
        config.permission_mode = mode;
        CommandResult::ConfigChangeMessage(config, describe(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_switches_it_the_other_way() {
        assert_eq!(parse_request(""), YoloRequest::Toggle);
        assert_eq!(parse_request("   "), YoloRequest::Toggle);
    }

    #[test]
    fn every_spelling_of_on_and_off_is_accepted() {
        for arg in ["on", "ON", "enable"] {
            assert_eq!(parse_request(arg), YoloRequest::On, "{arg}");
        }
        for arg in ["off", "OFF", "disable"] {
            assert_eq!(parse_request(arg), YoloRequest::Off, "{arg}");
        }
        assert_eq!(parse_request("status"), YoloRequest::Show);
    }

    #[test]
    fn a_word_that_is_not_a_setting_is_refused_rather_than_guessed() {
        assert_eq!(
            parse_request("maybe"),
            YoloRequest::Invalid("maybe".to_string())
        );
    }

    #[test]
    fn the_toggle_runs_both_ways() {
        assert_eq!(
            next_mode(&YoloRequest::Toggle, PermissionMode::Default),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            next_mode(&YoloRequest::Toggle, PermissionMode::BypassPermissions),
            PermissionMode::Default
        );
    }

    #[test]
    fn switching_on_from_any_mode_reaches_bypass() {
        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::Plan,
        ] {
            assert_eq!(
                next_mode(&YoloRequest::Toggle, mode),
                PermissionMode::BypassPermissions,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn switching_off_lands_on_default_rather_than_guessing() {
        // Nothing records the mode that preceded bypass, and guessing
        // accept-edits would hand back more than was taken away.
        assert_eq!(
            next_mode(&YoloRequest::Off, PermissionMode::BypassPermissions),
            PermissionMode::Default
        );
    }

    #[test]
    fn the_description_names_the_risk_rather_than_only_the_state() {
        let on = describe(PermissionMode::BypassPermissions);
        assert!(on.contains("without asking"), "{on:?}");
        assert!(on.contains("shell"), "{on:?}");
        // The two modes that are neither yolo nor plain default say which they
        // are, so "off" is never mistaken for "asks about everything".
        assert!(describe(PermissionMode::AcceptEdits).contains("accept-edits"));
        assert!(describe(PermissionMode::Plan).contains("plan mode"));
    }

    /// `MIKMIK_HOME` is process-global, so the tests that redirect it run one
    /// at a time and put it back afterwards.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        saved: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn pointing_at(dir: &std::path::Path) -> Self {
            let saved = std::env::var_os("MIKMIK_HOME");
            std::env::set_var("MIKMIK_HOME", dir);
            Self { saved }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("MIKMIK_HOME", value),
                None => std::env::remove_var("MIKMIK_HOME"),
            }
        }
    }

    fn ctx() -> CommandContext {
        CommandContext {
            context_window: 200_000,
            context_used_tokens: 0,
            config: mikmik_core::Config::default(),
            cost_tracker: mikmik_core::cost::CostTracker::new(),
            messages: vec![],
            working_dir: std::path::PathBuf::from("."),
            session_id: "test-session".to_string(),
            session_title: None,
            effort_level: None,
            remote_session_url: None,
            mcp_manager: None,
            mcp_auth_runner: None,
            interactive: true,
            active_agent: None,
        }
    }

    fn saved_mode() -> PermissionMode {
        mikmik_core::Settings::load_sync()
            .expect("settings load")
            .config
            .permission_mode
    }

    #[tokio::test]
    async fn the_mode_reaches_the_file_and_not_only_the_session() {
        // Shift+Tab already cycles the mode for one session. Surviving a
        // restart is the whole reason this command exists.
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = YoloCommand.execute("on", &mut ctx()).await;
        assert!(matches!(result, CommandResult::ConfigChangeMessage(..)));
        assert_eq!(saved_mode(), PermissionMode::BypassPermissions);

        YoloCommand.execute("off", &mut ctx()).await;
        assert_eq!(saved_mode(), PermissionMode::Default);
    }

    #[tokio::test]
    async fn no_second_settings_key_is_written() {
        // The whole point of the shape: one key describes the state, so a
        // settings file cannot say two contradictory things about it.
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        YoloCommand.execute("on", &mut ctx()).await;
        let written = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("settings file written");

        assert!(written.contains("bypassPermissions"), "{written}");
        assert!(!written.to_lowercase().contains("yolo"), "{written}");
    }

    #[tokio::test]
    async fn showing_the_mode_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = YoloCommand.execute("status", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Message(_)));
        assert!(!dir.path().join("settings.json").exists());
    }

    #[tokio::test]
    async fn an_argument_that_is_not_a_setting_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = YoloCommand.execute("maybe", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Error(_)));
        assert!(!dir.path().join("settings.json").exists());
    }
}
