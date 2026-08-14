// `/buddy` — the companion that sits beside the input box.
//
// The companion has two halves. Its bones (species, rarity, eye, hat, shiny,
// stats) come from `claurst-buddy`, rolled deterministically from the user's
// identity, so they are the same on every run and cannot be edited into
// something rarer by hand. Its soul (name and personality) is written once by
// a model on the first hatch and then kept in `companion.json`.

use super::*;
use async_trait::async_trait;
use claurst_buddy::{Companion, CompanionSoul};

pub struct BuddyCommand;

#[async_trait]
impl SlashCommand for BuddyCommand {
    fn name(&self) -> &str {
        "buddy"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["companion"]
    }

    fn description(&self) -> &str {
        "Show the companion beside the input box, or turn it on and off"
    }

    fn help(&self) -> &str {
        "Usage: /buddy [on|off|forget]\n\n\
         A small creature sits beside the input box. Its species, rarity and\n\
         stats are rolled from your identity and are the same every run; its\n\
         name and personality are written once, on the first hatch, and then\n\
         kept in `companion.json`.\n\n\
         Address it by name in a message and it answers in a speech bubble.\n\n\
         Examples:\n\
           /buddy         show the companion, hatching it if it is new\n\
           /buddy on      show the companion and tell the model about it\n\
           /buddy off     hide it and stop describing it to the model\n\
           /buddy forget  discard the name and personality, keeping the bones"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        match args.trim() {
            "" => show(ctx).await,
            "on" => set_enabled(ctx, true),
            "off" => set_enabled(ctx, false),
            "forget" => forget(),
            other => CommandResult::Error(format!(
                "'{other}' is not something /buddy does. Try `on`, `off`, `forget`, or nothing at all."
            )),
        }
    }
}

/// Turn the companion on or off and persist the choice.
fn set_enabled(ctx: &CommandContext, enabled: bool) -> CommandResult {
    // Load and save through the typed `Settings` path, so keys this struct
    // does not model survive the write.
    let mut settings = match claurst_core::config::Settings::load_sync() {
        Ok(settings) => settings,
        Err(e) => {
            return CommandResult::Error(format!(
                "Could not read settings: {e}. Fix the file, then try again."
            ))
        }
    };

    let mut companion = settings.companion.take().unwrap_or_default();
    companion.enabled = enabled;
    settings.companion = Some(companion);

    if let Err(e) = settings.save_sync() {
        return CommandResult::Error(format!("Could not save settings: {e}"));
    }

    let message = if enabled {
        "Companion on. It appears beside the input box, and the model is told it is there."
    } else {
        "Companion off."
    };
    CommandResult::ConfigChangeMessage(ctx.config.clone(), message.to_string())
}

/// Discard the stored soul. The bones are untouched because they are not
/// stored in the first place.
fn forget() -> CommandResult {
    let path = claurst_core::claurst_home().join("companion.json");
    match std::fs::remove_file(&path) {
        Ok(()) => CommandResult::Message(
            "Companion forgotten. The next `/buddy` hatches it again, with the same body \
             and a new name."
                .to_string(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            CommandResult::Message("There is no hatched companion to forget.".to_string())
        }
        Err(e) => CommandResult::Error(format!("Could not remove {}: {e}", path.display())),
    }
}

/// Show the companion card, hatching it first if it has never been named.
async fn show(ctx: &CommandContext) -> CommandResult {
    let config_dir = claurst_core::claurst_home();
    let identity = claurst_core::accounts::stable_identity();
    let mut companion = claurst_buddy::get_companion(&identity, &config_dir);

    // An unhatched companion is shown either way. The bones exist without the
    // model, so a provider that is not reachable costs the name, not the card.
    let mut note = String::new();
    if companion.soul.is_none() {
        match hatch(ctx, &companion).await {
            Ok(soul) => {
                if let Err(e) = claurst_buddy::save_companion_soul(&config_dir, &soul) {
                    note = format!("\n\nHatched, but could not save companion.json: {e}");
                }
                companion.soul = Some(soul);
            }
            Err(e) => {
                note = format!(
                    "\n\nNot hatched yet, so it has no name: {e}\nThe body above is already \
                     decided and will not change."
                );
            }
        }
    }

    CommandResult::Message(format!("{}{note}", card(&companion)))
}

/// Render the companion card: sprite, identity line, and stats.
fn card(companion: &Companion) -> String {
    let bones = &companion.bones;
    let mut out = String::new();

    // The sprite reserves a top row for a hat or for per-frame flourishes, and
    // pads every row to a fixed width. The card holds one still frame, so a
    // row of spaces there is just a gap.
    let sprite = claurst_buddy::render(companion, 0);
    let rows: Vec<&str> = sprite
        .lines()
        .skip_while(|row| row.trim().is_empty())
        .collect();
    out.push_str(&rows.join("\n"));
    out.push_str("\n\n");

    let shiny = if bones.shiny { " ✨shiny" } else { "" };
    // `display_name` falls back to the species, which would read as
    // "mushroom the common mushroom" before the companion has a name.
    let subject = match &companion.soul {
        Some(soul) => format!("{} the", soul.name),
        None => "an unhatched".to_string(),
    };
    out.push_str(&format!(
        "{subject} {} {} {}{}\n",
        bones.rarity.as_str(),
        bones.species.as_str(),
        bones.rarity.stars(),
        shiny
    ));

    if bones.hat != claurst_buddy::Hat::None {
        out.push_str(&format!("wearing: {}\n", hat_name(&bones.hat)));
    }

    if let Some(soul) = &companion.soul {
        out.push_str(&format!(
            "hatched: {}\n{}\n",
            soul.hatched_at.format("%Y-%m-%d"),
            soul.personality
        ));
    }

    let stats = &bones.stats;
    out.push_str("\ndebugging  ");
    out.push_str(&stat_bar(stats.debugging));
    out.push_str("\npatience   ");
    out.push_str(&stat_bar(stats.patience));
    out.push_str("\nchaos      ");
    out.push_str(&stat_bar(stats.chaos));
    out.push_str("\nwisdom     ");
    out.push_str(&stat_bar(stats.wisdom));
    out.push_str("\nsnark      ");
    out.push_str(&stat_bar(stats.snark));

    out
}

/// A 20-cell bar plus the number, so the card reads at a glance and still
/// gives the exact value.
fn stat_bar(value: u8) -> String {
    let filled = (value as usize * 20) / 100;
    format!(
        "{}{} {value:>3}",
        "█".repeat(filled),
        "·".repeat(20 - filled)
    )
}

fn hat_name(hat: &claurst_buddy::Hat) -> &'static str {
    use claurst_buddy::Hat;
    match hat {
        Hat::None => "nothing",
        Hat::Crown => "a crown",
        Hat::Tophat => "a top hat",
        Hat::Propeller => "a propeller cap",
        Hat::Halo => "a halo",
        Hat::Wizard => "a wizard hat",
        Hat::Beanie => "a beanie",
        Hat::TinyDuck => "a tiny duck",
    }
}

/// Ask a model to name the companion and describe it in one line.
///
/// The bones are handed over as context so the name fits the body: a
/// legendary dragon and a common snail should not read the same.
async fn hatch(ctx: &CommandContext, companion: &Companion) -> Result<CompanionSoul, String> {
    let bones = &companion.bones;
    let model = companion_model(&ctx.config);

    let provider = claurst_api::provider_for_config(&ctx.config)
        .await
        .ok_or("no provider is configured to hatch with")?;

    let request = claurst_api::ProviderRequest {
        model: model.clone(),
        messages: vec![Message::user(format!(
            "Name this creature and describe it.\n\n\
             species: {}\nrarity: {}\nhat: {}\n\
             debugging {} / patience {} / chaos {} / wisdom {} / snark {}\n\n\
             Reply with exactly two lines and nothing else:\n\
             line 1: the name, one or two words, no punctuation\n\
             line 2: its personality in under 12 words, lower case, no period",
            bones.species.as_str(),
            bones.rarity.as_str(),
            hat_name(&bones.hat),
            bones.stats.debugging,
            bones.stats.patience,
            bones.stats.chaos,
            bones.stats.wisdom,
            bones.stats.snark,
        ))],
        system_prompt: Some(claurst_api::SystemPrompt::Text(
            "You name small imaginary creatures that live in a terminal. The name should \
             suit the body and the stats. Be specific and odd, never generic."
                .to_string(),
        )),
        tools: vec![],
        max_tokens: 128,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: vec![],
        thinking: None,
        provider_options: serde_json::Value::Object(Default::default()),
    };

    let response = provider
        .create_message(request)
        .await
        .map_err(|e| format!("the hatching call failed: {e}"))?;

    ctx.cost_tracker.add_usage(
        &model,
        response.usage.input_tokens,
        response.usage.output_tokens,
        response.usage.cache_creation_input_tokens,
        response.usage.cache_read_input_tokens,
    );

    let text = text_from_content_blocks(&response.content);
    parse_soul(&text).ok_or_else(|| format!("model '{model}' returned no name"))
}

/// The model that hatches the companion and writes its bubble lines.
pub(crate) fn companion_model(config: &Config) -> String {
    config
        .companion
        .as_ref()
        .and_then(|companion| companion.model.clone())
        .unwrap_or_else(|| config.effective_model().to_string())
}

/// Read the two-line hatching reply.
///
/// A model that ignores the format still hatches something: the first
/// non-empty line becomes the name. Refusing here would leave the companion
/// nameless over a formatting slip.
fn parse_soul(text: &str) -> Option<CompanionSoul> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.trim_start_matches(['-', '*', '#']).trim());

    let name = lines.next().filter(|name| !name.is_empty())?;
    // A long first line means the model wrote prose instead of a name; take
    // its first two words rather than putting a paragraph in the sprite label.
    let name: String = if name.chars().count() > 24 {
        name.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        name.to_string()
    };

    let personality = lines
        .next()
        .unwrap_or("keeps its own counsel")
        .trim_matches('.')
        .to_string();

    Some(CompanionSoul {
        name,
        personality,
        hatched_at: chrono::Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::cost::CostTracker;

    /// Serve exactly one OpenAI-shaped chat completion, then stop.
    ///
    /// The hatch path goes out over HTTP, so a real socket is the only way to
    /// prove the request is built and the reply is read. Written by hand
    /// rather than with a mock crate to keep the dev-dependency list short.
    async fn one_shot_openai(content: &str) -> String {
        let body = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 0,
            "model": "mock-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 120, "completion_tokens": 14, "total_tokens": 134 },
        })
        .to_string();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let addr = listener.local_addr().expect("read the bound port");

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // Read until the headers end; the body length does not matter
            // because the response is fixed.
            let mut seen = Vec::new();
            let mut chunk = [0u8; 1024];
            while let Ok(n) = socket.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&chunk[..n]);
                if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        });

        format!("http://{addr}/v1")
    }

    fn ctx_pointing_at(base_url: String) -> CommandContext {
        let mut config = Config {
            provider: Some("openai".to_string()),
            model: Some("mock-model".to_string()),
            ..Default::default()
        };
        config.provider_configs.insert(
            "openai".to_string(),
            claurst_core::config::ProviderConfig {
                api_key: Some("sk-test".to_string()),
                api_base: Some(base_url),
                ..Default::default()
            },
        );

        CommandContext {
            config,
            cost_tracker: CostTracker::new(),
            messages: vec![],
            working_dir: std::path::PathBuf::from("."),
            session_id: "test-session".to_string(),
            session_title: None,
            effort_level: None,
            remote_session_url: None,
            mcp_manager: None,
            mcp_auth_runner: None,
        }
    }

    #[tokio::test]
    async fn hatching_names_the_companion_and_bills_the_call() {
        let base_url = one_shot_openai("Mossback\nnaps through every outage").await;
        let ctx = ctx_pointing_at(base_url);
        let companion = Companion::new("test-identity", None);

        let soul = hatch(&ctx, &companion).await.expect("the mock answers");
        assert_eq!(soul.name, "Mossback");
        assert_eq!(soul.personality, "naps through every outage");

        // The companion spends the user's tokens, so it has to show up in
        // `/cost` under the model that spent them.
        let spend = ctx.cost_tracker.by_model();
        assert_eq!(spend.len(), 1);
        assert_eq!(spend[0].model, "mock-model");
        assert_eq!(spend[0].tokens, 134);
    }

    #[tokio::test]
    async fn a_provider_that_is_not_there_leaves_the_companion_unhatched() {
        // No provider_configs and no key: nothing to call.
        let ctx = CommandContext {
            config: Config::default(),
            cost_tracker: CostTracker::new(),
            messages: vec![],
            working_dir: std::path::PathBuf::from("."),
            session_id: "test-session".to_string(),
            session_title: None,
            effort_level: None,
            remote_session_url: None,
            mcp_manager: None,
            mcp_auth_runner: None,
        };
        let companion = Companion::new("test-identity", None);

        let error = hatch(&ctx, &companion).await.expect_err("nothing to call");
        assert!(error.contains("provider"), "unhelpful error: {error}");

        // The card is still printable, because the body never needed a model.
        assert!(card(&companion).contains(companion.bones.species.as_str()));
    }

    #[test]
    fn an_unnamed_companion_is_not_named_after_its_own_species() {
        // `display_name` falls back to the species, which would print
        // "mushroom the common mushroom".
        let companion = Companion::new("test-identity", None);
        let species = companion.bones.species.as_str();
        assert!(!card(&companion).contains(&format!("{species} the")));
        assert!(card(&companion).contains("an unhatched"));
    }

    #[test]
    fn the_card_opens_on_the_sprite_and_not_on_a_blank_row() {
        // Every sprite pads its rows to a fixed width, and reserves the top
        // one for a hat. A bare companion would otherwise open with a row of
        // spaces that reads as a stray gap.
        for identity in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            let companion = Companion::new(identity, None);
            let first = card(&companion).lines().next().unwrap_or("").to_string();
            assert!(!first.trim().is_empty(), "blank first row for {identity}");
        }
    }

    #[test]
    fn a_two_line_reply_becomes_a_soul() {
        let soul = parse_soul("Quackers\nchaotic, helpful, slightly damp").expect("should parse");
        assert_eq!(soul.name, "Quackers");
        assert_eq!(soul.personality, "chaotic, helpful, slightly damp");
    }

    #[test]
    fn blank_lines_and_list_markers_are_ignored() {
        let soul = parse_soul("\n- Mossback\n\n* naps through every outage\n").expect("parses");
        assert_eq!(soul.name, "Mossback");
        assert_eq!(soul.personality, "naps through every outage");
    }

    #[test]
    fn a_model_that_writes_prose_still_hatches_something() {
        // Nothing here matches the requested format. The companion is named
        // anyway, because a nameless companion is worse than an odd name.
        let soul = parse_soul("Sure, here is a name for your creature").expect("parses");
        assert_eq!(soul.name, "Sure, here");
        assert_eq!(soul.personality, "keeps its own counsel");
    }

    #[test]
    fn an_empty_reply_hatches_nothing() {
        assert!(parse_soul("").is_none());
        assert!(parse_soul("   \n\n  ").is_none());
    }

    #[test]
    fn the_card_shows_the_body_the_rarity_and_every_stat() {
        let companion = Companion::new("test-identity", None);
        let rendered = card(&companion);

        assert!(rendered.contains(companion.bones.species.as_str()));
        assert!(rendered.contains(companion.bones.rarity.as_str()));
        for stat in ["debugging", "patience", "chaos", "wisdom", "snark"] {
            assert!(rendered.contains(stat), "missing stat: {stat}");
        }
    }

    #[test]
    fn a_hatched_companion_shows_its_name_and_personality() {
        let mut companion = Companion::new("test-identity", None);
        companion.soul = Some(CompanionSoul {
            name: "Quackers".to_string(),
            personality: "chaotic, helpful, slightly damp".to_string(),
            hatched_at: chrono::Utc::now(),
        });

        let rendered = card(&companion);
        assert!(rendered.contains("Quackers"));
        assert!(rendered.contains("chaotic, helpful, slightly damp"));
    }

    #[test]
    fn the_stat_bar_fills_in_proportion() {
        assert!(stat_bar(0).starts_with('·'));
        assert!(stat_bar(100).starts_with(&"█".repeat(20)));
        assert!(stat_bar(50).contains(&"█".repeat(10)));
        // The number is always readable, whatever the bar looks like.
        assert!(stat_bar(7).ends_with("  7"));
    }
}
