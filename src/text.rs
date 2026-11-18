use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

pub fn course_key(name: &str) -> String {
    let plain: String = name
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
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
}
