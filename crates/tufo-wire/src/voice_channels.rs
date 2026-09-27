//! The process-neutral contract for listing voice channels one page at a time.

use tufo_models::discord::{VoiceChannel, VoiceChannelTarget};

/// How many voice channels one page holds: 1 to 500.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceChannelPageSize(usize);

impl VoiceChannelPageSize {
    pub const DEFAULT: Self = Self(100);
    pub const MAX: Self = Self(500);

    /// Interprets a requested size: zero selects the default and larger sizes are capped.
    #[must_use]
    pub fn from_requested(requested: usize) -> Self {
        match requested {
            0 => Self::DEFAULT,
            requested => Self(requested.min(Self::MAX.0)),
        }
    }

    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

/// One page of the voice channels, ordered by guild id and then channel id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListVoiceChannels {
    /// The last channel of the previous page, or `None` for the first page.
    pub after: Option<VoiceChannelTarget>,
    pub page_size: VoiceChannelPageSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChannelPage {
    pub channels: Vec<VoiceChannel>,
    /// The cursor for the next page, present only when more channels follow.
    pub next_after: Option<VoiceChannelTarget>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_page_sizes_default_and_cap() {
        assert_eq!(
            VoiceChannelPageSize::from_requested(0),
            VoiceChannelPageSize::DEFAULT
        );
        assert_eq!(VoiceChannelPageSize::from_requested(7).get(), 7);
        assert_eq!(
            VoiceChannelPageSize::from_requested(10_000),
            VoiceChannelPageSize::MAX
        );
    }
}
