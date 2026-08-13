// advisor_target.rs — parse the `advisorModel` setting into a provider, an
// optional account profile, and a model id.
//
// The parser lives here rather than next to either caller because both
// `claurst-tools` (the Advisor tool) and `claurst-commands` (`/advisor`) need
// the same reading of the setting, and they cannot depend on each other.

use std::fmt;

/// Where an advisor call should be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorTarget<'a> {
    /// Provider to call: the one named in the setting, or the session's.
    pub provider_id: &'a str,
    /// Account profile to authenticate as, when the setting names one.
    pub profile_id: Option<&'a str>,
    /// Model id to pass to the provider.
    pub model: &'a str,
}

/// Why an `advisorModel` value could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisorTargetError {
    /// The setting is empty or only whitespace.
    Empty,
    /// A `:` appeared in the provider part with an empty side, so neither the
    /// provider nor the profile can be recovered.
    MalformedProfile,
}

impl fmt::Display for AdvisorTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the advisor model is empty"),
            Self::MalformedProfile => f.write_str(
                "expected `provider:account/model`, with a non-empty provider and account",
            ),
        }
    }
}

impl std::error::Error for AdvisorTargetError {}

/// Read an `advisorModel` value.
///
/// Accepted forms, in the order they are tried:
///
/// | Setting                     | provider          | profile    | model         |
/// |-----------------------------|-------------------|------------|---------------|
/// | `sonnet`                    | `active_provider` | none       | `sonnet`      |
/// | `openai/gpt-4o`             | `openai`          | none       | `gpt-4o`      |
/// | `anthropic:personal/sonnet` | `anthropic`       | `personal` | `sonnet`      |
///
/// The split on `/` happens first and only the first `/` separates, so a model
/// id keeps any further slashes (`openrouter/meta/llama-3`). The `:` is then
/// looked for only in the provider part, so a colon inside a model id
/// (`ollama/llama3:8b`) is never mistaken for an account.
pub fn parse_advisor_model<'a>(
    configured: &'a str,
    active_provider: &'a str,
) -> Result<AdvisorTarget<'a>, AdvisorTargetError> {
    let configured = configured.trim();
    if configured.is_empty() {
        return Err(AdvisorTargetError::Empty);
    }

    let Some((head, model)) = configured.split_once('/') else {
        // A bare model id runs on the session's provider and active account.
        return Ok(AdvisorTarget {
            provider_id: active_provider,
            profile_id: None,
            model: configured,
        });
    };

    // A leading or trailing `/` leaves one side empty; treat the whole string
    // as a model id so `/foo` is not read as an empty provider.
    if head.is_empty() || model.is_empty() {
        return Ok(AdvisorTarget {
            provider_id: active_provider,
            profile_id: None,
            model: configured,
        });
    }

    match head.split_once(':') {
        Some((provider_id, profile_id)) => {
            if provider_id.is_empty() || profile_id.is_empty() {
                return Err(AdvisorTargetError::MalformedProfile);
            }
            Ok(AdvisorTarget {
                provider_id,
                profile_id: Some(profile_id),
                model,
            })
        }
        None => Ok(AdvisorTarget {
            provider_id: head,
            profile_id: None,
            model,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(configured: &str) -> Result<AdvisorTarget<'_>, AdvisorTargetError> {
        parse_advisor_model(configured, "anthropic")
    }

    #[test]
    fn a_bare_model_runs_on_the_session_provider() {
        let target = parse("sonnet").expect("parses");
        assert_eq!(target.provider_id, "anthropic");
        assert_eq!(target.profile_id, None);
        assert_eq!(target.model, "sonnet");
    }

    #[test]
    fn a_provider_prefix_selects_that_provider() {
        let target = parse("openai/gpt-4o").expect("parses");
        assert_eq!(target.provider_id, "openai");
        assert_eq!(target.profile_id, None);
        assert_eq!(target.model, "gpt-4o");
    }

    #[test]
    fn a_colon_in_the_provider_part_selects_an_account() {
        let target = parse("anthropic:personal/sonnet").expect("parses");
        assert_eq!(target.provider_id, "anthropic");
        assert_eq!(target.profile_id, Some("personal"));
        assert_eq!(target.model, "sonnet");
    }

    #[test]
    fn a_colon_inside_a_model_id_is_not_an_account() {
        let target = parse("ollama/llama3:8b").expect("parses");
        assert_eq!(target.provider_id, "ollama");
        assert_eq!(target.profile_id, None);
        assert_eq!(target.model, "llama3:8b");
    }

    #[test]
    fn only_the_first_slash_separates_the_model() {
        let target = parse("openrouter/meta/llama-3").expect("parses");
        assert_eq!(target.provider_id, "openrouter");
        assert_eq!(target.profile_id, None);
        assert_eq!(target.model, "meta/llama-3");
    }

    #[test]
    fn a_bare_model_with_a_colon_stays_a_model() {
        let target = parse("llama3:8b").expect("parses");
        assert_eq!(target.provider_id, "anthropic");
        assert_eq!(target.profile_id, None);
        assert_eq!(target.model, "llama3:8b");
    }

    #[test]
    fn an_empty_account_is_rejected() {
        assert_eq!(
            parse("anthropic:/sonnet"),
            Err(AdvisorTargetError::MalformedProfile)
        );
    }

    #[test]
    fn an_empty_provider_before_an_account_is_rejected() {
        assert_eq!(
            parse(":personal/sonnet"),
            Err(AdvisorTargetError::MalformedProfile)
        );
    }

    #[test]
    fn an_empty_setting_is_rejected() {
        assert_eq!(parse(""), Err(AdvisorTargetError::Empty));
        assert_eq!(parse("   "), Err(AdvisorTargetError::Empty));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let target = parse("  anthropic:personal/sonnet  ").expect("parses");
        assert_eq!(target.provider_id, "anthropic");
        assert_eq!(target.profile_id, Some("personal"));
        assert_eq!(target.model, "sonnet");
    }

    #[test]
    fn a_dangling_slash_is_treated_as_a_model_id() {
        // Not an account form, so it must not be silently read as a provider.
        let target = parse("sonnet/").expect("parses");
        assert_eq!(target.provider_id, "anthropic");
        assert_eq!(target.model, "sonnet/");
    }
}
