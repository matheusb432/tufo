use std::io::IsTerminal as _;

use clap::{
    ColorChoice,
    builder::styling::{AnsiColor, Style},
};

#[derive(Clone, Copy)]
pub(crate) struct Console {
    color: bool,
    color_choice: ColorChoice,
}

impl Console {
    pub(crate) fn from_terminal() -> Self {
        let color_choice = if std::env::var_os("NO_COLOR").is_some() {
            ColorChoice::Never
        } else if std::env::var_os("CLICOLOR_FORCE")
            .is_some_and(|value| value != "0" && !value.is_empty())
        {
            ColorChoice::Always
        } else {
            ColorChoice::Auto
        };
        let color = match color_choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::io::stdout().is_terminal()
                    && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
            }
        };
        Self {
            color,
            color_choice,
        }
    }

    pub(crate) const fn color_choice(self) -> ColorChoice {
        self.color_choice
    }

    pub(crate) fn heading(self, section: &str) -> String {
        format!(
            "{}  /  {section}\n",
            self.paint(
                "TUFO",
                Style::new().bold().fg_color(Some(AnsiColor::Green.into()))
            )
        )
    }

    pub(crate) fn row(self, badge: &str, label: &str, detail: &str) -> String {
        let color = match badge {
            "OK" => AnsiColor::Green,
            "WARN" => AnsiColor::Yellow,
            "FAIL" => AnsiColor::Red,
            _ => AnsiColor::Cyan,
        };
        let badge = self.paint(
            &format!("{badge:>4}"),
            Style::new().bold().fg_color(Some(color.into())),
        );
        format!("  {badge}  {label:<14}  {}", single_line(detail))
    }

    fn paint(self, text: &str, style: Style) -> String {
        if self.color {
            format!("{style}{text}{style:#}")
        } else {
            text.to_owned()
        }
    }
}

/// Keep paths and remote values from injecting extra rows or terminal controls.
pub(crate) fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}
