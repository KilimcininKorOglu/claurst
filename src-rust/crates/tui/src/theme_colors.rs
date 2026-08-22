// theme_colors.rs — Color palette management for accessibility-friendly themes.
//
// Provides color definitions for different themes, with special support for
// Deuteranopia (red-green color blindness) using blue, yellow, and gray palettes.

use ratatui::style::Color;

/// Color palette for a specific theme.
///
/// `Copy` because it is eleven `Color`s and nothing else: rendering reads it
/// every frame, and a clone at each call site would be noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorPalette {
    /// Error messages and alerts (normally red, but color-blind friendly)
    pub error: Color,
    /// Success indicators (normally green, but color-blind friendly)
    pub success: Color,
    /// Warning/caution messages
    pub warning: Color,
    /// Information messages
    pub info: Color,
    /// Action buttons and interactive elements
    pub action: Color,
    /// Disabled or dimmed states
    pub disabled: Color,
    /// Primary accent color
    pub accent: Color,
    /// Secondary accent
    pub secondary_accent: Color,
    /// Text on dark backgrounds
    pub text_light: Color,
    /// Text on light backgrounds
    pub text_dark: Color,
    /// Borders and dividers
    pub border: Color,
}

impl ColorPalette {
    /// Get the color palette for a given theme name.
    pub fn for_theme(theme_name: &str) -> Self {
        match theme_name {
            "deuteranopia" => Self::deuteranopia(),
            "dark" => Self::dark(),
            "light" => Self::light(),
            "solarized" => Self::solarized(),
            "nord" => Self::nord(),
            "dracula" => Self::dracula(),
            "monokai" => Self::monokai(),
            _ => Self::default_theme(),
        }
    }

    /// Default MikMik theme
    fn default_theme() -> Self {
        Self {
            error: Color::Rgb(255, 87, 51),   // Bright red-orange
            success: Color::Rgb(76, 175, 80), // Green
            warning: Color::Rgb(255, 152, 0), // Orange
            info: Color::Cyan,
            action: Color::Cyan,
            disabled: Color::DarkGray,
            accent: Color::Cyan,
            secondary_accent: Color::Rgb(233, 30, 99), // Magenta
            text_light: Color::White,
            text_dark: Color::Black,
            border: Color::DarkGray,
        }
    }

    /// Dark theme
    fn dark() -> Self {
        Self {
            error: Color::Rgb(239, 83, 80),     // Light red
            success: Color::Rgb(129, 199, 132), // Light green
            warning: Color::Rgb(255, 171, 64),  // Light orange
            info: Color::Rgb(100, 181, 246),    // Light blue
            action: Color::Rgb(100, 181, 246),
            disabled: Color::Rgb(97, 97, 97),
            accent: Color::Rgb(100, 181, 246),
            secondary_accent: Color::Rgb(229, 57, 53),
            text_light: Color::Rgb(229, 229, 229),
            text_dark: Color::Rgb(33, 33, 33),
            border: Color::Rgb(66, 66, 66),
        }
    }

    /// Light theme
    fn light() -> Self {
        Self {
            error: Color::Rgb(211, 47, 47),    // Dark red
            success: Color::Rgb(27, 94, 32),   // Dark green
            warning: Color::Rgb(230, 124, 13), // Dark orange
            info: Color::Rgb(13, 71, 161),     // Dark blue
            action: Color::Blue,
            disabled: Color::Rgb(189, 189, 189),
            accent: Color::Blue,
            secondary_accent: Color::Rgb(194, 24, 91),
            text_light: Color::White,
            text_dark: Color::Black,
            border: Color::Rgb(189, 189, 189),
        }
    }

    /// Solarized Dark theme
    fn solarized() -> Self {
        Self {
            error: Color::Rgb(220, 50, 47),   // Solarized red
            success: Color::Rgb(133, 153, 0), // Solarized green
            warning: Color::Rgb(181, 137, 0), // Solarized yellow
            info: Color::Rgb(38, 139, 210),   // Solarized blue
            action: Color::Rgb(38, 139, 210),
            disabled: Color::Rgb(88, 110, 117),
            accent: Color::Rgb(38, 139, 210),
            secondary_accent: Color::Rgb(108, 113, 196),
            text_light: Color::Rgb(131, 148, 150),
            text_dark: Color::Rgb(0, 43, 54),
            border: Color::Rgb(7, 54, 66),
        }
    }

    /// Nord theme
    fn nord() -> Self {
        Self {
            error: Color::Rgb(191, 97, 106),    // Nord red
            success: Color::Rgb(163, 190, 140), // Nord green
            warning: Color::Rgb(235, 203, 139), // Nord yellow
            info: Color::Rgb(136, 192, 208),    // Nord blue
            action: Color::Rgb(136, 192, 208),
            disabled: Color::Rgb(76, 86, 106),
            accent: Color::Rgb(136, 192, 208),
            secondary_accent: Color::Rgb(191, 97, 106),
            text_light: Color::Rgb(236, 239, 244),
            text_dark: Color::Rgb(46, 52, 64),
            border: Color::Rgb(67, 76, 94),
        }
    }

    /// Dracula theme
    fn dracula() -> Self {
        Self {
            error: Color::Rgb(255, 85, 85),     // Dracula red
            success: Color::Rgb(80, 250, 123),  // Dracula green
            warning: Color::Rgb(241, 250, 140), // Dracula yellow
            info: Color::Rgb(139, 233, 253),    // Dracula blue
            action: Color::Rgb(139, 233, 253),
            disabled: Color::Rgb(98, 114, 164),
            accent: Color::Rgb(139, 233, 253),
            secondary_accent: Color::Rgb(189, 147, 249),
            text_light: Color::Rgb(248, 248, 242),
            text_dark: Color::Rgb(40, 42, 54),
            border: Color::Rgb(68, 71, 90),
        }
    }

    /// Monokai theme
    fn monokai() -> Self {
        Self {
            error: Color::Rgb(249, 38, 114), // Monokai magenta (used for errors)
            success: Color::Rgb(166, 226, 46), // Monokai green
            warning: Color::Rgb(253, 151, 31), // Monokai orange
            info: Color::Rgb(102, 217, 239), // Monokai cyan
            action: Color::Rgb(102, 217, 239),
            disabled: Color::Rgb(117, 113, 94),
            accent: Color::Rgb(102, 217, 239),
            secondary_accent: Color::Rgb(249, 38, 114),
            text_light: Color::Rgb(248, 248, 242),
            text_dark: Color::Rgb(39, 40, 34),
            border: Color::Rgb(75, 75, 75),
        }
    }

    /// Deuteranopia (red-green color blind) theme
    /// Uses blue, yellow, and gray to avoid red/green distinction
    fn deuteranopia() -> Self {
        Self {
            error: Color::Rgb(255, 140, 0),   // Orange (not red)
            success: Color::Rgb(0, 150, 200), // Blue (not green)
            warning: Color::Rgb(255, 180, 0), // Gold/Yellow
            info: Color::Cyan,
            action: Color::Rgb(0, 150, 200), // Blue action buttons
            disabled: Color::Rgb(120, 120, 120), // Neutral gray
            accent: Color::Rgb(0, 150, 200), // Blue accent
            secondary_accent: Color::Rgb(180, 140, 255), // Purple accent
            text_light: Color::Rgb(220, 220, 220),
            text_dark: Color::Rgb(40, 40, 40),
            border: Color::Rgb(100, 100, 100),
        }
    }
}

impl ColorPalette {
    /// The palette a `Theme` selects.
    ///
    /// A custom theme falls back to the default palette rather than failing:
    /// the name may belong to a theme the picker offers but this table has no
    /// colours for.
    pub fn for_config_theme(theme: &mikmik_core::config::Theme) -> Self {
        use mikmik_core::config::Theme;
        Self::for_theme(match theme {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Deuteranopia => "deuteranopia",
            Theme::Default => "default",
            Theme::Custom(name) => name.as_str(),
        })
    }

    /// The colour that marks who or what a line came from.
    pub fn message_indicator(&self, role: &str) -> Color {
        match role {
            "user" => self.accent,
            "assistant" => self.secondary_accent,
            "system" => self.disabled,
            "tool" => self.action,
            _ => self.text_light,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikmik_core::config::Theme;

    #[test]
    fn deuteranopia_separates_the_pair_red_green_blindness_confuses() {
        // The whole point of the theme: an error and a success that a
        // deuteranopic reader cannot tell apart are the same message.
        let default = ColorPalette::for_theme("default");
        let deuteranopia = ColorPalette::for_theme("deuteranopia");

        assert_ne!(deuteranopia.error, default.error);
        assert_ne!(deuteranopia.success, default.success);
        assert_ne!(
            deuteranopia.error, deuteranopia.success,
            "error and success must stay distinguishable"
        );
    }

    #[test]
    fn every_theme_the_picker_offers_resolves_to_a_palette() {
        for theme in [
            Theme::Default,
            Theme::Dark,
            Theme::Light,
            Theme::Deuteranopia,
            Theme::Custom("dracula".to_string()),
            // A name with no colours of its own falls back rather than failing.
            Theme::Custom("no-such-theme".to_string()),
        ] {
            let palette = ColorPalette::for_config_theme(&theme);
            assert_ne!(
                palette.error, palette.success,
                "{theme:?} must not paint error and success the same"
            );
        }
    }

    #[test]
    fn a_custom_name_with_no_colours_falls_back_to_the_default_palette() {
        assert_eq!(
            ColorPalette::for_config_theme(&Theme::Custom("no-such-theme".to_string())),
            ColorPalette::for_theme("default")
        );
    }

    #[test]
    fn each_role_gets_its_own_indicator() {
        let palette = ColorPalette::for_theme("default");
        assert_eq!(palette.message_indicator("user"), palette.accent);
        assert_eq!(
            palette.message_indicator("assistant"),
            palette.secondary_accent
        );
        assert_eq!(palette.message_indicator("tool"), palette.action);
        assert_eq!(
            palette.message_indicator("anything else"),
            palette.text_light
        );
    }
}
