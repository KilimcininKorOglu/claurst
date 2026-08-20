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
    settings.companion = Some(companion.clone());

    if let Err(e) = settings.save_sync() {
        return CommandResult::Error(format!("Could not save settings: {e}"));
    }

    // The live `Config` carries the same value, and the running session reads
    // it from there. Writing only the settings file would leave the companion
    // off until the next launch.
    let mut config = ctx.config.clone();
    config.companion = Some(companion);

    let message = if enabled {
        "Companion on. It appears beside the input box, and the model is told it is there."
    } else {
        "Companion off."
    };
    CommandResult::ConfigChangeMessage(config, message.to_string())
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
    let mut hatched = false;
    if companion.soul.is_none() {
        match hatch(ctx, &companion).await {
            Ok(soul) => {
                if let Err(e) = claurst_buddy::save_companion_soul(&config_dir, &soul) {
                    note = format!("\n\nHatched, but could not save companion.json: {e}");
                }
                companion.soul = Some(soul);
                hatched = true;
            }
            Err(e) => {
                note = format!(
                    "\n\nNot hatched yet, so it has no name: {e}\nThe body above is already \
                     decided and will not change."
                );
            }
        }
    }

    let card = format!("{}{note}", card(&companion));
    if hatched {
        // A hatch changes what the session caches: the sprite beside the input
        // box and the name the model is told to watch for. Report it as a
        // config change so both are picked up without a restart.
        CommandResult::ConfigChangeMessage(ctx.config.clone(), card)
    } else {
        CommandResult::Message(card)
    }
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
    let route = companion_route(&ctx.config);

    let provider = claurst_api::provider_for_account(&ctx.config, &route.account)
        .await
        .map_err(|e| format!("no provider is configured to hatch with: {e}"))?;

    let request = claurst_api::ProviderRequest {
        model: route.model.clone(),
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
        route.model.as_str(),
        response.usage.input_tokens,
        response.usage.output_tokens,
        response.usage.cache_creation_input_tokens,
        response.usage.cache_read_input_tokens,
    );

    let text = text_from_content_blocks(&response.content);
    parse_soul(&text).ok_or_else(|| format!("model '{}' returned no name", route.model))
}

/// Write one line for the companion to say, in reply to the user's message.
///
/// Called only when the user addressed the companion by name; there is no
/// idle chatter, because every line here is a model call the user pays for.
/// The companion is a watcher, not the agent: it is told what was said, not
/// given the transcript or any tools.
pub async fn companion_reply(
    config: &Config,
    cost_tracker: &std::sync::Arc<CostTracker>,
    companion: &Companion,
    user_message: &str,
) -> Result<String, String> {
    let soul = companion
        .soul
        .as_ref()
        .ok_or("the companion has no name yet")?;
    let route = companion_route(config);

    let provider = claurst_api::provider_for_account(config, &route.account)
        .await
        .map_err(|e| format!("no provider is configured: {e}"))?;

    let request = claurst_api::ProviderRequest {
        model: route.model.clone(),
        messages: vec![Message::user(format!(
            "The user just said:\n\n{}",
            truncate_for_bubble(user_message)
        ))],
        system_prompt: Some(claurst_api::SystemPrompt::Text(format!(
            "You are {}, a small {} sitting beside a programmer's terminal. You are \
             {}. The user said something to you. Answer in ONE short line, under 15 \
             words, lower case, no quotes and no preamble. You are not the coding \
             assistant and you do not do the work: you watch, and you have opinions.",
            soul.name,
            companion.bones.species.as_str(),
            soul.personality,
        ))),
        tools: vec![],
        max_tokens: 96,
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
        .map_err(|e| format!("the companion could not answer: {e}"))?;

    cost_tracker.add_usage(
        route.model.as_str(),
        response.usage.input_tokens,
        response.usage.output_tokens,
        response.usage.cache_creation_input_tokens,
        response.usage.cache_read_input_tokens,
    );

    let text = text_from_content_blocks(&response.content);
    first_line(&text).ok_or_else(|| format!("model '{}' said nothing", route.model))
}

/// Keep the prompt small: the companion reacts to what was said, and a pasted
/// stack trace would cost far more than the line it produces is worth.
fn truncate_for_bubble(message: &str) -> String {
    const LIMIT: usize = 600;
    if message.chars().count() <= LIMIT {
        return message.to_string();
    }
    let head: String = message.chars().take(LIMIT).collect();
    format!("{head}…")
}

/// The first non-empty line, stripped of the quoting a model tends to add.
fn first_line(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .trim_matches(['"', '\'', '*'])
        .trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The account and model that hatch the companion and write its bubble lines.
///
/// A `Route`, because the companion's own `model` setting may name an account
/// (`"cheap_account/haiku"`) and the request has to reach that account with
/// the prefix removed. It used to go out whole, to whichever account the
/// session had selected.
pub(crate) fn companion_route(config: &Config) -> claurst_core::config::Route {
    match config
        .companion
        .as_ref()
        .and_then(|companion| companion.model.as_deref())
        .filter(|model| !model.is_empty())
    {
        Some(model) => config.resolve_route(model),
        None => config.effective_route(),
    }
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

    /// `CLAURST_HOME` is process-global, so the tests that redirect it run one
    /// at a time and put it back afterwards. Async-aware because those tests
    /// hold it across the model call.
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
            interactive: true,
            active_agent: None,
        }
    }

    fn hatched(name: &str) -> Companion {
        let mut companion = Companion::new("test-identity", None);
        companion.soul = Some(CompanionSoul {
            name: name.to_string(),
            personality: "naps through every outage".to_string(),
            hatched_at: chrono::Utc::now(),
        });
        companion
    }

    #[tokio::test]
    async fn a_reply_is_one_line_and_is_billed_to_the_companion_model() {
        let base_url = one_shot_openai("\"you broke it again\"\nand another line").await;
        let ctx = ctx_pointing_at(base_url);

        let line = claurst_commands_reply(&ctx, &hatched("Mossback"), "mossback, thoughts?")
            .await
            .expect("the mock answers");
        // Quotes stripped, second line dropped: the bubble is one row and the
        // model habitually wraps short replies in quotes.
        assert_eq!(line, "you broke it again");

        let spend = ctx.cost_tracker.by_model();
        assert_eq!(spend.len(), 1);
        assert_eq!(spend[0].model, "mock-model");
    }

    #[tokio::test]
    async fn an_unhatched_companion_cannot_answer() {
        // No socket is opened: this must fail before any provider call.
        let ctx = ctx_pointing_at("http://127.0.0.1:1/v1".to_string());
        let error = claurst_commands_reply(&ctx, &Companion::new("test-identity", None), "hello")
            .await
            .expect_err("nameless companions stay quiet");
        assert!(error.contains("no name"), "unhelpful error: {error}");
    }

    /// Thin wrapper so the tests read the same as the call site.
    async fn claurst_commands_reply(
        ctx: &CommandContext,
        companion: &Companion,
        said: &str,
    ) -> Result<String, String> {
        companion_reply(&ctx.config, &ctx.cost_tracker, companion, said).await
    }

    #[tokio::test]
    async fn the_command_hatches_once_and_reads_from_disk_after_that() {
        let home = tempfile::tempdir().expect("temp home");
        let _lock = HOME_LOCK.lock().await;
        let _guard = HomeGuard::pointing_at(home.path());

        // One socket, so a second model call would fail outright.
        let base_url = one_shot_openai("Mossback\nnaps through every outage").await;
        let mut ctx = ctx_pointing_at(base_url);

        let first = BuddyCommand.execute("", &mut ctx).await;
        let first = match first {
            // A hatch changes cached session state, so it reports a config change.
            CommandResult::ConfigChangeMessage(_, text) => text,
            other => panic!("expected a hatch, got {other:?}"),
        };
        assert!(first.contains("Mossback the"), "not hatched: {first}");
        assert!(home.path().join("companion.json").exists());

        // Second call: the dead socket proves this came off disk.
        let second = BuddyCommand.execute("", &mut ctx).await;
        let second = match second {
            CommandResult::Message(text) => text,
            other => panic!("expected a plain card, got {other:?}"),
        };
        assert!(second.contains("Mossback the"), "not re-read: {second}");
        assert_eq!(ctx.cost_tracker.by_model().len(), 1, "hatched twice");

        // Forgetting removes the name and leaves the body alone.
        let forgotten = BuddyCommand.execute("forget", &mut ctx).await;
        assert!(matches!(forgotten, CommandResult::Message(_)));
        assert!(!home.path().join("companion.json").exists());
    }

    #[tokio::test]
    async fn on_and_off_are_written_where_the_session_and_the_next_launch_both_read() {
        let home = tempfile::tempdir().expect("temp home");
        let _lock = HOME_LOCK.lock().await;
        let _guard = HomeGuard::pointing_at(home.path());

        let mut ctx = ctx_pointing_at("http://127.0.0.1:1/v1".to_string());

        match BuddyCommand.execute("on", &mut ctx).await {
            CommandResult::ConfigChangeMessage(config, _) => {
                // The live config, so the running session sees it.
                assert!(config.companion.expect("set on Config").enabled);
            }
            other => panic!("expected a config change, got {other:?}"),
        }
        // And the settings file, so the next launch sees it too.
        let settings = claurst_core::config::Settings::load_sync().expect("read back");
        assert!(settings.companion.expect("written to disk").enabled);

        match BuddyCommand.execute("off", &mut ctx).await {
            CommandResult::ConfigChangeMessage(config, _) => {
                assert!(!config.companion.expect("set on Config").enabled);
            }
            other => panic!("expected a config change, got {other:?}"),
        }
        let settings = claurst_core::config::Settings::load_sync().expect("read back");
        assert!(!settings.companion.expect("written to disk").enabled);
    }

    #[test]
    fn a_long_message_is_cut_before_it_is_sent() {
        // A pasted stack trace would cost more than the one line it buys.
        let long = "x".repeat(5_000);
        let sent = truncate_for_bubble(&long);
        assert!(sent.chars().count() < 700);
        assert!(sent.ends_with('…'));

        let short = "mossback, thoughts?";
        assert_eq!(truncate_for_bubble(short), short);
    }

    #[test]
    fn a_reply_wrapped_in_chatter_is_reduced_to_its_first_line() {
        assert_eq!(first_line("  \n\n  hm.  \nmore"), Some("hm.".to_string()));
        assert_eq!(first_line("*sighs*"), Some("sighs".to_string()));
        assert_eq!(first_line("   \n  "), None);
        assert_eq!(first_line(""), None);
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
            interactive: true,
            active_agent: None,
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
