//! Kenku FM's `cleanFileName`, which ends in `Case.capital` from the npm `case` package 1.6.
//! Every class below lies in the BMP, so walking `char`s matches JavaScript's UTF-16.

/// Such as `" Calm Mist"` for `[calm]-mist.ogg`.
#[must_use]
pub(super) fn kenku_default_title(file_name: &str) -> String {
    let stem = match file_name.rfind('.') {
        Some(dot) if dot > 0 => &file_name[..dot],
        _ => file_name,
    };
    let collapsed = collapse_spaces(stem);
    capital(collapsed.trim_matches(is_js_whitespace))
}

fn collapse_spaces(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    for character in text.chars() {
        if !(character == ' ' && collapsed.ends_with(' ')) {
            collapsed.push(character);
        }
    }
    collapsed
}

/// `Case.capital`.
fn capital(text: &str) -> String {
    let prepared = prep(text);
    let characters = prepared.chars().collect::<Vec<_>>();
    let mut capitalized = String::with_capacity(prepared.len());
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if index == 0 && is_lower(character) {
            capitalized.extend(character.to_uppercase());
            index += 1;
        } else if is_symbol(character)
            && characters
                .get(index + 1)
                .is_some_and(|&next| is_lower(next))
        {
            capitalized.push(character);
            capitalized.extend(characters[index + 1].to_uppercase());
            index += 2;
        } else {
            capitalized.push(character);
            index += 1;
        }
    }
    capitalized
}

/// `Case._.prep` without options.
fn prep(text: &str) -> String {
    let mut prepared = if !text.is_empty() && !text.chars().any(is_lower) {
        text.to_lowercase()
    } else {
        text.to_owned()
    };
    if !has_hole(&prepared) {
        let holey = fill_with_spaces(&prepared);
        if has_hole(&holey) {
            prepared = holey;
        }
    }
    if !prepared.chars().any(is_symbol) {
        prepared = relax(&prepared);
    }
    prepared
}

/// `/[^\s]\s[^\s]/`.
fn has_hole(text: &str) -> bool {
    let characters = text.chars().collect::<Vec<_>>();
    characters.windows(3).any(|window| {
        !is_js_whitespace(window[0]) && is_js_whitespace(window[1]) && !is_js_whitespace(window[2])
    })
}

/// `Case._.fill(s, ' ')`, whose `.` can't match a line terminator.
fn fill_with_spaces(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut filled = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        if !is_symbol(characters[index]) {
            filled.push(characters[index]);
            index += 1;
            continue;
        }
        let run_start = index;
        while index < characters.len() && is_symbol(characters[index]) {
            index += 1;
        }
        match characters.get(index) {
            None => {}
            Some(&next) if !is_line_terminator(next) => {
                filled.push(' ');
                filled.push(next);
                index += 1;
            }
            Some(_) if index - run_start >= 2 => {
                filled.push(' ');
                filled.push(characters[index - 1]);
            }
            Some(_) => filled.push(characters[run_start]),
        }
    }
    filled
}

/// `Case._.relax` over `/([^U])([U]*)([U])(?=[^U]|$)/g`.
fn relax(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut relaxed = String::with_capacity(text.len() + 8);
    let mut index = 0;
    while index < characters.len() {
        let before = characters[index];
        let run_end = characters[index + 1..]
            .iter()
            .position(|&character| !is_upper(character))
            .map_or(characters.len(), |offset| index + 1 + offset);
        if is_upper(before) || run_end == index + 1 {
            relaxed.push(before);
            index += 1;
            continue;
        }
        relaxed.push(before);
        relaxed.push(' ');
        let run = &characters[index + 1..run_end];
        let (acronym, capital) = run.split_at(run.len() - 1);
        if !acronym.is_empty() {
            relaxed.extend(acronym);
            relaxed.push(' ');
        }
        relaxed.extend(capital);
        index = run_end;
    }
    relaxed
}

/// `case`'s symbols, which leave out the apostrophe.
fn is_symbol(character: char) -> bool {
    matches!(
        character,
        '\u{20}'..='\u{26}'
            | '\u{28}'..='\u{2F}'
            | '\u{3A}'..='\u{40}'
            | '\u{5B}'..='\u{60}'
            | '\u{7B}'..='\u{7E}'
            | '\u{A0}'..='\u{BF}'
            | '\u{D7}'
            | '\u{F7}'
    )
}

fn is_lower(character: char) -> bool {
    matches!(character, 'a'..='z' | '\u{DF}'..='\u{F6}' | '\u{F8}'..='\u{FF}')
}

fn is_upper(character: char) -> bool {
    matches!(character, 'A'..='Z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{DE}')
}

/// JavaScript's `\s`: Unicode's white space without NEL, plus the BOM.
fn is_js_whitespace(character: char) -> bool {
    character == '\u{FEFF}' || (character.is_whitespace() && character != '\u{85}')
}

fn is_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::kenku_default_title;

    #[test]
    fn symbols_become_spaces_before_capitalized_words() {
        assert_eq!(
            kenku_default_title("[calm]-mist-over-the-old-bridge.ogg"),
            " Calm Mist Over The Old Bridge"
        );
        assert_eq!(
            kenku_default_title("uivo-lobo_distante.mp3"),
            "Uivo Lobo Distante"
        );
    }

    #[test]
    fn spaced_names_keep_their_symbols() {
        assert_eq!(
            kenku_default_title("the  tavern (night).mp3"),
            "The Tavern (Night)"
        );
        assert_eq!(kenku_default_title("[sad]-low-horn.MP3"), " Sad Low Horn");
    }

    #[test]
    fn camel_case_and_shouting_split_into_words() {
        assert_eq!(kenku_default_title("myHTTPServer.wav"), "My HTTP Server");
        assert_eq!(kenku_default_title("DRAGON_ROAR.wav"), "Dragon Roar");
        assert_eq!(kenku_default_title("çava ßtraße.mp3"), "Çava SStraße");
    }

    #[test]
    fn only_the_last_extension_goes() {
        assert_eq!(kenku_default_title("boss.theme.ogg"), "Boss Theme");
        assert_eq!(kenku_default_title(".hidden"), ".Hidden");
        assert_eq!(kenku_default_title("trovao"), "Trovao");
    }
}
