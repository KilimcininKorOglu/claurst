// Poke command: read or change whether unfinished todos nudge the model
// between turns (`/poke`).

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;

pub struct PokeCommand;

/// What `/poke <args>` asks for.
#[derive(Debug, PartialEq, Eq)]
enum PokeRequest {
    /// No argument: report whether the nudge is on.
    Show,
    /// Send the nudge.
    On,
    /// Stop sending it.
    Off,
    /// Return to the configured default, which is on.
    Reset,
    /// The argument made no sense; the string names what was wrong.
    Invalid(String),
}

fn parse_request(args: &str) -> PokeRequest {
    let args = args.trim();
    if args.is_empty() || args.eq_ignore_ascii_case("status") {
        return PokeRequest::Show;
    }
    if args.eq_ignore_ascii_case("on") || args.eq_ignore_ascii_case("enable") {
        return PokeRequest::On;
    }
    if args.eq_ignore_ascii_case("off") || args.eq_ignore_ascii_case("disable") {
        return PokeRequest::Off;
    }
    if args.eq_ignore_ascii_case("default") || args.eq_ignore_ascii_case("reset") {
        return PokeRequest::Reset;
    }
    PokeRequest::Invalid(args.to_string())
}

/// How the setting in force reads.
///
/// An unset value and an explicit `true` both send the nudge, so they must not
/// read differently: someone who sees "unset" would otherwise go looking for a
/// switch that is already in the position they want.
fn describe(auto_poke: Option<bool>) -> String {
    match auto_poke {
        Some(false) => "Auto-poke is off: unfinished todos never nudge the model.".to_string(),
        Some(true) => {
            "Auto-poke is on: unfinished todos nudge the model between turns.".to_string()
        }
        None => {
            "Auto-poke is on (default): unfinished todos nudge the model between turns.".to_string()
        }
    }
}

#[async_trait]
impl SlashCommand for PokeCommand {
    fn name(&self) -> &str {
        "poke"
    }

    fn description(&self) -> &str {
        "Show or change whether unfinished todos nudge the model"
    }

    fn help(&self) -> &str {
        "Usage:\n\
         /poke            — show whether the nudge is on\n\
         /poke on         — nudge the model about unfinished todos\n\
         /poke off        — stop nudging\n\
         /poke default    — go back to the configured default (on)\n\n\
         After a turn that leaves todos unfinished, claurst appends a short\n\
         reminder listing what is left so the run continues instead of\n\
         stopping halfway. Turn it off for a session where you drive each\n\
         step yourself. The setting is saved as `autoPoke` in settings."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let (auto_poke, note) = match parse_request(args) {
            PokeRequest::Show => {
                return CommandResult::Message(describe(ctx.config.auto_poke));
            }
            PokeRequest::Invalid(arg) => {
                return CommandResult::Error(format!(
                    "{:?} is not an auto-poke setting. Use `on`, `off`, or `default`.",
                    arg
                ));
            }
            PokeRequest::On => (Some(true), describe(Some(true))),
            PokeRequest::Off => (Some(false), describe(Some(false))),
            PokeRequest::Reset => (None, describe(None)),
        };

        // The file takes the change before the session does, so a failed write
        // cannot report a setting it saved nowhere.
        if let Err(e) = super::save_settings_mutation(|s| s.config.auto_poke = auto_poke) {
            return CommandResult::Error(format!("Could not save the auto-poke setting: {}", e));
        }

        let mut config = ctx.config.clone();
        config.auto_poke = auto_poke;
        CommandResult::ConfigChangeMessage(config, note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_asks_to_see_the_setting() {
        assert_eq!(parse_request(""), PokeRequest::Show);
        assert_eq!(parse_request("   "), PokeRequest::Show);
        assert_eq!(parse_request("status"), PokeRequest::Show);
    }

    #[test]
    fn every_spelling_of_on_and_off_is_accepted() {
        for arg in ["on", "ON", "enable"] {
            assert_eq!(parse_request(arg), PokeRequest::On, "{arg}");
        }
        for arg in ["off", "OFF", "disable"] {
            assert_eq!(parse_request(arg), PokeRequest::Off, "{arg}");
        }
    }

    #[test]
    fn default_and_reset_clear_the_override() {
        assert_eq!(parse_request("default"), PokeRequest::Reset);
        assert_eq!(parse_request("Reset"), PokeRequest::Reset);
    }

    #[test]
    fn a_word_that_is_not_a_setting_is_refused_rather_than_guessed() {
        assert_eq!(
            parse_request("maybe"),
            PokeRequest::Invalid("maybe".to_string())
        );
        // `true`/`false` are not offered, so they must not be guessed at either.
        assert_eq!(
            parse_request("true"),
            PokeRequest::Invalid("true".to_string())
        );
    }

    #[test]
    fn an_unset_value_reads_as_on_rather_than_as_unset() {
        // Both send the nudge. Reporting "unset" would send someone looking for
        // a switch that is already where they want it.
        assert!(describe(None).contains("on"));
        assert!(describe(Some(true)).contains("on"));
        assert!(describe(Some(false)).contains("off"));
    }

    /// `CLAURST_HOME` is process-global, so the tests that redirect it run one
    /// at a time and put it back afterwards.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        saved: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn pointing_at(dir: &std::path::Path) -> Self {
            let saved = std::env::var_os("CLAURST_HOME");
            std::env::set_var("CLAURST_HOME", dir);
            Self { saved }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("CLAURST_HOME", value),
                None => std::env::remove_var("CLAURST_HOME"),
            }
        }
    }

    fn ctx() -> CommandContext {
        CommandContext {
            config: claurst_core::Config::default(),
            cost_tracker: claurst_core::cost::CostTracker::new(),
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

    fn saved_auto_poke() -> Option<bool> {
        claurst_core::Settings::load_sync()
            .expect("settings load")
            .config
            .auto_poke
    }

    #[tokio::test]
    async fn the_setting_reaches_the_file_and_not_only_the_session() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = PokeCommand.execute("off", &mut ctx()).await;
        assert!(matches!(result, CommandResult::ConfigChangeMessage(..)));
        assert_eq!(saved_auto_poke(), Some(false));

        PokeCommand.execute("on", &mut ctx()).await;
        assert_eq!(saved_auto_poke(), Some(true));

        PokeCommand.execute("default", &mut ctx()).await;
        assert_eq!(
            saved_auto_poke(),
            None,
            "the default is the absence of the key, not a written copy of it"
        );
    }

    #[tokio::test]
    async fn showing_the_setting_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        PokeCommand.execute("off", &mut ctx()).await;
        let result = PokeCommand.execute("", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Message(_)));
        assert_eq!(saved_auto_poke(), Some(false), "still what was set before");
    }

    #[tokio::test]
    async fn an_argument_that_is_not_a_setting_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = PokeCommand.execute("maybe", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Error(_)));
        assert_eq!(saved_auto_poke(), None);
    }
}
