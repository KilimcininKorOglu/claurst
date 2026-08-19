// Turns command: read or change this session's agentic turn limit (`/turns`).

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;
use claurst_core::constants::{MAX_TURNS_DEFAULT, MAX_TURNS_UNLIMITED};

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
         active."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let (max_turns, note) = match parse_request(args) {
            TurnsRequest::Show => {
                return CommandResult::Message(describe(ctx.config.max_turns));
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

    #[test]
    fn the_description_names_the_default_and_the_absence_of_a_limit() {
        assert!(describe(None).contains(&MAX_TURNS_DEFAULT.to_string()));
        assert!(describe(None).contains("default"));
        assert_eq!(describe(Some(MAX_TURNS_UNLIMITED)), "Max turns: no limit.");
        assert_eq!(describe(Some(25)), "Max turns: 25.");
    }
}
