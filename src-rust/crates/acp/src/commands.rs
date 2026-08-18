//! The slash commands a connected client may offer, in the protocol's terms.
//!
//! The command layer is the same one the terminal runs, so an editor gets the
//! whole set rather than a second, smaller list that would drift from it.

use agent_client_protocol_schema as acp;

/// Every command a client may show, with the hint for what it takes.
///
/// Hidden commands are left out: they exist for compatibility or for tests,
/// and a client offering them would be advertising things nobody should type.
pub fn available_commands() -> Vec<acp::AvailableCommand> {
    claurst_commands::all_commands()
        .iter()
        .filter(|command| !command.hidden())
        .map(|command| {
            acp::AvailableCommand::new(command.name(), command.description()).input(Some(
                acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                    input_hint(command.help(), command.name()),
                )),
            ))
        })
        .collect()
}

/// What to show in the input box before anything is typed.
///
/// Taken from the command's own usage line, which is where the argument form
/// is written. Without one there is nothing to promise, so the generic word is
/// used rather than a guess at what the command accepts.
fn input_hint(help: &str, name: &str) -> String {
    let Some(usage) = help.lines().next().and_then(|l| l.strip_prefix("Usage:")) else {
        return "arguments".to_string();
    };
    let usage = usage.trim();
    let arguments = usage
        .strip_prefix(&format!("/{name}"))
        .unwrap_or(usage)
        .trim();
    if arguments.is_empty() {
        return "arguments".to_string();
    }
    arguments.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offered_commands_are_the_ones_the_terminal_runs() {
        let offered = available_commands();
        let expected = claurst_commands::all_commands()
            .iter()
            .filter(|c| !c.hidden())
            .count();

        assert_eq!(offered.len(), expected);
        assert!(offered.iter().any(|c| c.name == "help"));
    }

    #[test]
    fn a_hidden_command_is_not_offered() {
        // Offering one would put a command in the client's list that nobody
        // is meant to type.
        let hidden: Vec<String> = claurst_commands::all_commands()
            .iter()
            .filter(|c| c.hidden())
            .map(|c| c.name().to_string())
            .collect();

        let offered = available_commands();
        for name in &hidden {
            assert!(
                !offered.iter().any(|c| &c.name == name),
                "hidden command {name} was offered"
            );
        }
    }

    #[test]
    fn every_offered_command_says_what_it_does() {
        for command in available_commands() {
            assert!(
                !command.description.is_empty(),
                "{} has no description",
                command.name
            );
        }
    }

    #[test]
    fn the_hint_comes_from_the_commands_own_usage_line() {
        assert_eq!(input_hint("Usage: /rewind [n]\nmore text", "rewind"), "[n]");
        assert_eq!(input_hint("Usage: /clear", "clear"), "arguments");
        assert_eq!(input_hint("Clears the conversation.", "clear"), "arguments");
    }
}
