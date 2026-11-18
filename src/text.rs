use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

pub fn course_key(name: &str) -> String {
    let plain: String = name
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

pub fn fnv64(input: &str) -> String {
    let hash = input.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn course_key_ignores_accents_case_and_spacing() {
        assert_eq!(
            course_key("  Análise   e Desenvolvimento de SISTEMAS "),
            course_key("analise e desenvolvimento de sistemas")
        );
    }

    #[test]
    fn different_courses_have_different_keys() {
        assert_ne!(course_key("A"), course_key("B"));
    }

    #[test]
    fn fnv64_is_stable() {
        assert_eq!(fnv64(""), "cbf29ce484222325");
    }

    #[test]
    fn one_line_collapses_and_cuts() {
        assert_eq!(one_line("  a \n\n b\t c ", 20), "a b c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("áéíóú", 5), "áéíóú");
    }
}
