/// Spinner verbs displayed during processing.
pub const SPINNER_VERBS: &[&str] = &[
    "Accomplishing",
    "Actioning",
    "Actualizing",
    "Architecting",
    "Baking",
    "Beaming",
    "Beboppin'",
    "Befuddling",
    "Billowing",
    "Blanching",
    "Bloviating",
    "Boogieing",
    "Boondoggling",
    "Booping",
    "Bootstrapping",
    "Brewing",
    "Bunning",
    "Burrowing",
    "Calculating",
    "Canoodling",
    "Caramelizing",
    "Cascading",
    "Catapulting",
    "Cerebrating",
    "Channeling",
    "Choreographing",
    "Churning",
    "Clauding",
    "Coalescing",
    "Cogitating",
    "Combobulating",
    "Composing",
    "Computing",
    "Concocting",
    "Considering",
    "Contemplating",
    "Cooking",
    "Crafting",
    "Creating",
    "Crunching",
    "Crystallizing",
    "Cultivating",
    "Deciphering",
    "Deliberating",
    "Determining",
    "Dilly-dallying",
    "Discombobulating",
    "Doing",
    "Doodling",
    "Drizzling",
    "Ebbing",
    "Effecting",
    "Elucidating",
    "Embellishing",
    "Enchanting",
    "Envisioning",
    "Evaporating",
    "Fermenting",
    "Fiddle-faddling",
    "Finagling",
    "Flambéing",
    "Flibbertigibbeting",
    "Flowing",
    "Flummoxing",
    "Fluttering",
    "Forging",
    "Forming",
    "Frolicking",
    "Frosting",
    "Gallivanting",
    "Galloping",
    "Garnishing",
    "Generating",
    "Gesticulating",
    "Germinating",
    "Gitifying",
    "Grooving",
    "Gusting",
    "Harmonizing",
    "Hashing",
    "Hatching",
    "Herding",
    "Honking",
    "Hullaballooing",
    "Hyperspacing",
    "Ideating",
    "Imagining",
    "Improvising",
    "Incubating",
    "Inferring",
    "Infusing",
    "Ionizing",
    "Jitterbugging",
    "Julienning",
    "Kneading",
    "Leavening",
    "Levitating",
    "Lollygagging",
    "Manifesting",
    "Marinating",
    "Meandering",
    "Metamorphosing",
    "Misting",
    "Moonwalking",
    "Moseying",
    "Mulling",
    "Mustering",
    "Musing",
    "Nebulizing",
    "Nesting",
    "Newspapering",
    "Noodling",
    "Nucleating",
    "Orbiting",
    "Orchestrating",
    "Osmosing",
    "Perambulating",
    "Percolating",
    "Perusing",
    "Philosophising",
    "Photosynthesizing",
    "Pollinating",
    "Pondering",
    "Pontificating",
    "Pouncing",
    "Precipitating",
    "Prestidigitating",
    "Processing",
    "Proofing",
    "Propagating",
    "Puttering",
    "Puzzling",
    "Quantumizing",
    "Razzle-dazzling",
    "Razzmatazzing",
    "Recombobulating",
    "Reticulating",
    "Roosting",
    "Ruminating",
    "Sautéing",
    "Scampering",
    "Schlepping",
    "Scurrying",
    "Seasoning",
    "Shenaniganing",
    "Shimmying",
    "Simmering",
    "Skedaddling",
    "Sketching",
    "Slithering",
    "Smooshing",
    "Sock-hopping",
    "Spelunking",
    "Spinning",
    "Sprouting",
    "Stewing",
    "Sublimating",
    "Swirling",
    "Swooping",
    "Symbioting",
    "Synthesizing",
    "Tempering",
    "Thinking",
    "Thundering",
    "Tinkering",
    "Tomfoolering",
    "Topsy-turvying",
    "Transfiguring",
    "Transmuting",
    "Twisting",
    "Undulating",
    "Unfurling",
    "Unravelling",
    "Vibing",
    "Waddling",
    "Wandering",
    "Warping",
    "Whatchamacalliting",
    "Whirlpooling",
    "Whirring",
    "Whisking",
    "Wibbling",
    "Working",
    "Wrangling",
    "Zesting",
    "Zigzagging",
    // Cat wordplay (MikMik) — cohesive with the completion verbs below.
    // Pouncing and Kneading are deliberately absent here: both already sit in
    // the neutral list above, and a repeat would double their odds.
    "Mousing",
    "Prowling",
    "Biscuit-making",
    "Purring",
    "Whiskering",
    "Stalking",
    "Pawing",
    "Slinking",
    "Grooming",
    "Perching",
    "Chirping",
    "Scratching",
    "Padding",
    "Tail-flicking",
    "Mouse-hunting",
    "Yarn-chasing",
    "Sunbeam-chasing",
    "Windowsill-sitting",
    "Loafing",
];

/// Past-tense verbs shown in the status row after a turn completes.
///
/// A mix of the neutral originals and a big pile of cat wordplay, in honour of
/// MikMik (mikmik's cat mascot) — so "Pounced for 2m 5s" and friends pad by
/// when a turn finishes.
pub const TURN_COMPLETION_VERBS: &[&str] = &[
    // Neutral.
    "Baked",
    "Brewed",
    "Churned",
    "Cogitated",
    "Cooked",
    "Crunched",
    "Pondered",
    "Processed",
    "Worked",
    // Cat wordplay (MikMik).
    "Pounced",
    "Prowled",
    "Kneaded",
    "Purred",
    "Whiskered",
    "Stalked",
    "Pawed",
    "Slunk",
    "Groomed",
    "Perched",
    "Chirped",
    "Scratched",
    "Padded",
    "Tail-flicked",
    "Mouse-hunted",
    "Yarn-chased",
    "Sunbeam-chased",
    "Windowsill-sat",
    "Loafed",
    "Catnapped",
    "Head-bonked",
    "Zoomied",
    "Bird-watched",
    "Box-sat",
    "Toe-beaned",
    "Blepped",
];

/// Select a random spinner verb.
pub fn sample_spinner_verb(seed: usize) -> &'static str {
    SPINNER_VERBS[seed % SPINNER_VERBS.len()]
}

/// Select a random completion verb.
pub fn sample_completion_verb(seed: usize) -> &'static str {
    TURN_COMPLETION_VERBS[seed % TURN_COMPLETION_VERBS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words that belonged to the crab mascot the cat replaced.
    const CRAB_TRACES: &[&str] = &[
        "Carapac",
        "Scuttl",
        "Molt",
        "Crab",
        "Chelat",
        "Barnacl",
        "Pincer",
        "Shell",
        "Clam",
        "Crustacea",
        "Tide-pool",
        "Reef",
        "Beachcomb",
        "Low-tide",
    ];

    #[test]
    fn no_verb_still_belongs_to_the_crab() {
        for verb in SPINNER_VERBS.iter().chain(TURN_COMPLETION_VERBS) {
            for trace in CRAB_TRACES {
                assert!(
                    !verb.contains(trace),
                    "{verb} still reads as the old crab mascot"
                );
            }
        }
    }

    #[test]
    fn the_cat_verbs_are_actually_there() {
        // Guards against someone deleting the wordplay instead of porting it.
        assert!(SPINNER_VERBS.contains(&"Mousing"));
        assert!(SPINNER_VERBS.contains(&"Loafing"));
        assert!(TURN_COMPLETION_VERBS.contains(&"Pounced"));
        assert!(TURN_COMPLETION_VERBS.contains(&"Zoomied"));
    }

    #[test]
    fn every_seed_lands_on_a_verb() {
        // Both samplers index with `seed % len()`, so an empty list would
        // panic on the first turn rather than at compile time.
        assert!(!SPINNER_VERBS.is_empty());
        assert!(!TURN_COMPLETION_VERBS.is_empty());
        for seed in 0..200 {
            assert!(!sample_spinner_verb(seed).is_empty());
            assert!(!sample_completion_verb(seed).is_empty());
        }
    }

    #[test]
    fn no_verb_is_listed_twice() {
        // A duplicate silently doubles that verb's odds.
        for list in [SPINNER_VERBS, TURN_COMPLETION_VERBS] {
            let unique: std::collections::HashSet<&&str> = list.iter().collect();
            assert_eq!(unique.len(), list.len(), "a verb is listed twice");
        }
    }
}
