// Turns command: read or change this session's agentic turn limit (`/turns`).

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;
use mikmik_core::constants::{MAX_TURNS_DEFAULT, MAX_TURNS_UNLIMITED};
use mikmik_core::AgentDefinition;

pub struct TurnsCommand;

/// What `/turns <args>` asks for.
#[derive(Debug, PartialEq, Eq)]
enum TurnsRequest {
    /// No argument: report the limit in force.
    Show,
    /// Set a specific ceiling.
    Set(u32),
    /// Remove the ceiling.
    Unlimited,
    /// Return to the configured default.
    Reset,
    /// The argument made no sense; the string names what was wrong.
    Invalid(String),
}

fn parse_request(args: &str) -> TurnsRequest {
    let args = args.trim();
    if args.is_empty() {
        return TurnsRequest::Show;
    }
    // `0` joins the words rather than meaning a zero-turn run, because a run
    // that may take no turns can do nothing at all.
    if args.eq_ignore_ascii_case("off")
        || args.eq_ignore_ascii_case("none")
        || args.eq_ignore_ascii_case("unlimited")
        || args == "0"
    {
        return TurnsRequest::Unlimited;
    }
    if args.eq_ignore_ascii_case("default") || args.eq_ignore_ascii_case("reset") {
        return TurnsRequest::Reset;
    }
    match args.parse::<u32>() {
        Ok(limit) => TurnsRequest::Set(limit),
        Err(_) => TurnsRequest::Invalid(args.to_string()),
    }
}

/// How the limit in force reads, given the configured value.
fn describe(max_turns: Option<u32>) -> String {
    match max_turns {
        Some(MAX_TURNS_UNLIMITED) => "Max turns: no limit.".to_string(),
        Some(limit) => format!("Max turns: {}.", limit),
        None => format!("Max turns: {} (default).", MAX_TURNS_DEFAULT),
    }
}

/// The note appended when the active agent's own limit wins over the session's.
///
/// Without it, setting a limit under such an agent looks like it took effect
/// and quietly does nothing until the agent is left.
fn agent_override_note(active_agent: Option<&AgentDefinition>) -> String {
    match active_agent.and_then(|agent| agent.max_turns) {
        Some(limit) => format!(
            " The active agent stops at {} turns, which wins until you leave it.",
            limit
        ),
        None => String::new(),
    }
}

#[async_trait]
impl SlashCommand for TurnsCommand {
    fn name(&self) -> &str {
        "turns"
    }

    fn description(&self) -> &str {
        "Show or change the agentic turn limit"
    }

    fn help(&self) -> &str {
        "Usage:\n\
         /turns              — show the limit in force\n\
         /turns <number>     — stop after that many agentic turns\n\
         /turns off          — no limit\n\
         /turns default      — go back to the configured default\n\n\
         The limit bounds how many turns one run may take before it stops. It\n\
         persists for the session and is saved as `maxTurns` in settings.\n\
         An agent definition's own `max_turns` still wins while that agent is\n\
         active, and `/turns` says so when one is."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let (max_turns, note) = match parse_request(args) {
            TurnsRequest::Show => {
                return CommandResult::Message(format!(
                    "{}{}",
                    describe(ctx.config.max_turns),
                    agent_override_note(ctx.active_agent.as_ref())
                ));
            }
            TurnsRequest::Invalid(arg) => {
                return CommandResult::Error(format!(
                    "{:?} is not a turn limit. Use a number, `off`, or `default`.",
                    arg
                ));
            }
            TurnsRequest::Set(limit) => (Some(limit), format!("Max turns set to {}.", limit)),
            TurnsRequest::Unlimited => (
                Some(MAX_TURNS_UNLIMITED),
                "Max turns: no limit.".to_string(),
            ),
            TurnsRequest::Reset => (
                None,
                format!("Max turns back to the default ({}).", MAX_TURNS_DEFAULT),
            ),
        };

        // The file takes the change before the session does. `ConfigChangeMessage`
        // only updates the configs held in memory, so without this the command
        // reports a limit it saved nowhere and the next launch is back at the
        // default.
        if let Err(e) = super::save_settings_mutation(|s| s.config.max_turns = max_turns) {
            return CommandResult::Error(format!("Could not save the turn limit: {}", e));
        }

        let note = format!("{}{}", note, agent_override_note(ctx.active_agent.as_ref()));
        let mut config = ctx.config.clone();
        config.max_turns = max_turns;
        CommandResult::ConfigChangeMessage(config, note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_asks_to_see_the_limit() {
        assert_eq!(parse_request(""), TurnsRequest::Show);
        assert_eq!(parse_request("   "), TurnsRequest::Show);
    }

    #[test]
    fn a_number_sets_the_limit() {
        assert_eq!(parse_request("25"), TurnsRequest::Set(25));
        assert_eq!(parse_request(" 3 "), TurnsRequest::Set(3));
    }

    #[test]
    fn every_spelling_of_no_limit_is_accepted() {
        for arg in ["off", "OFF", "none", "unlimited", "0"] {
            assert_eq!(parse_request(arg), TurnsRequest::Unlimited, "{arg}");
        }
    }

    #[test]
    fn zero_means_no_limit_rather_than_a_run_that_cannot_start() {
        // A zero-turn run would send no request at all, which is never what
        // someone typing `/turns 0` wants.
        assert_eq!(parse_request("0"), TurnsRequest::Unlimited);
    }

    #[test]
    fn default_and_reset_clear_the_override() {
        assert_eq!(parse_request("default"), TurnsRequest::Reset);
        assert_eq!(parse_request("Reset"), TurnsRequest::Reset);
    }

    #[test]
    fn a_word_that_is_not_a_limit_is_refused_rather_than_guessed() {
        assert_eq!(
            parse_request("lots"),
            TurnsRequest::Invalid("lots".to_string())
        );
        // A negative number is not a `u32`; refusing beats wrapping.
        assert_eq!(parse_request("-1"), TurnsRequest::Invalid("-1".to_string()));
    }

    fn agent_with(max_turns: Option<u32>) -> AgentDefinition {
        AgentDefinition {
            max_turns,
            ..Default::default()
        }
    }

    #[test]
    fn no_agent_adds_no_note() {
        assert_eq!(agent_override_note(None), "");
    }

    #[test]
    fn an_agent_without_its_own_limit_adds_no_note() {
        // Most agents define no limit, so the note must stay off by default.
        assert_eq!(agent_override_note(Some(&agent_with(None))), "");
    }

    #[test]
    fn an_agent_with_its_own_limit_says_which_number_wins() {
        // Setting a limit under such an agent otherwise looks like it took
        // effect and quietly does nothing.
        let note = agent_override_note(Some(&agent_with(Some(4))));
        assert!(note.contains('4'), "{note:?}");
        assert!(note.contains("wins"), "{note:?}");
    }

    #[test]
    fn the_description_names_the_default_and_the_absence_of_a_limit() {
        assert!(describe(None).contains(&MAX_TURNS_DEFAULT.to_string()));
        assert!(describe(None).contains("default"));
        assert_eq!(describe(Some(MAX_TURNS_UNLIMITED)), "Max turns: no limit.");
        assert_eq!(describe(Some(25)), "Max turns: 25.");
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

    /// What the settings file on disk holds for `maxTurns`.
    fn saved_max_turns() -> Option<u32> {
        mikmik_core::Settings::load_sync()
            .expect("settings load")
            .config
            .max_turns
    }

    #[tokio::test]
    async fn a_limit_reaches_the_settings_file_and_not_only_the_session() {
        // `ConfigChangeMessage` updates the three configs held in memory and
        // nothing else, so the command used to report a limit it saved nowhere.
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = TurnsCommand.execute("25", &mut ctx()).await;
        assert!(matches!(result, CommandResult::ConfigChangeMessage(..)));
        assert_eq!(saved_max_turns(), Some(25));

        TurnsCommand.execute("off", &mut ctx()).await;
        assert_eq!(saved_max_turns(), Some(MAX_TURNS_UNLIMITED));

        TurnsCommand.execute("default", &mut ctx()).await;
        assert_eq!(
            saved_max_turns(),
            None,
            "the default is the absence of the key, not a written copy of it"
        );
    }

    #[tokio::test]
    async fn showing_the_limit_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        TurnsCommand.execute("25", &mut ctx()).await;
        let result = TurnsCommand.execute("", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Message(_)));
        assert_eq!(saved_max_turns(), Some(25), "still what was set before");
    }

    #[tokio::test]
    async fn an_argument_that_is_not_a_limit_writes_nothing() {
        let _lock = HOME_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let _home = HomeGuard::pointing_at(dir.path());

        let result = TurnsCommand.execute("lots", &mut ctx()).await;

        assert!(matches!(result, CommandResult::Error(_)));
        assert_eq!(saved_max_turns(), None);
    }
}
