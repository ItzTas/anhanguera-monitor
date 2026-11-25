use std::fmt;
use std::io;
use std::str::FromStr;
use std::time::Duration;

use monitor_hub_sdk::settings::Problem;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::secret::{REDACTED, Secret};

pub const SETTINGS_JSON: &str = include_str!("../settings.json");

const MINIMUM_CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);
const MINIMUM_FAILING_ALERT_AFTER: Duration = Duration::from_secs(60 * 60);
const MAXIMUM_REMINDER_DAY: i64 = 60;
const USERNAME_DIGITS: usize = 11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub problems: Vec<Problem>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined: Vec<String> = self
            .problems
            .iter()
            .map(|p| match p.path.as_str() {
                "" => p.message.clone(),
                path => format!("{path}: {}", p.message),
            })
            .collect();
        f.write_str(&joined.join("; "))
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BillingDayNet {
    Auto,
    Fixed(u32),
    Off,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default)]
    database_url: Option<String>,
    #[serde(default)]
    database_url_file: Option<String>,
    #[serde(default)]
    ava_username: Option<String>,
    #[serde(default)]
    ava_password: Option<String>,
    #[serde(default)]
    ava_password_file: Option<String>,
    #[serde(default)]
    courses: Vec<String>,
    #[serde(default)]
    check_interval: Option<String>,
    #[serde(default)]
    payment_reminder_days: Vec<i64>,
    #[serde(default)]
    payment_overdue_repeat: Option<String>,
    #[serde(default)]
    boleto_link: Option<bool>,
    #[serde(default)]
    billing_day_net: Option<String>,
    #[serde(default)]
    deadline_reminder_days: Vec<i64>,
    #[serde(default)]
    calendar_categories: Vec<String>,
    #[serde(default)]
    reminder_hour: Option<i64>,
    #[serde(default)]
    failing_alert_after: Option<String>,
    #[serde(default)]
    ava_timezone: Option<String>,
    #[serde(default)]
    log_level: Option<String>,
    #[serde(default)]
    ava_graphql_url: Option<String>,
}

pub struct Config {
    pub database_url: Secret,
    pub username: Secret,
    pub password: Secret,
    pub courses: Vec<String>,
    pub check_interval: Duration,
    pub payment_reminder_days: Vec<u32>,
    pub payment_overdue_repeat: Option<Duration>,
    pub boleto_link: bool,
    pub billing_day_net: BillingDayNet,
    pub deadline_reminder_days: Vec<u32>,
    pub calendar_categories: Vec<String>,
    pub reminder_hour: u32,
    pub failing_alert_after: Duration,
    pub timezone: chrono_tz::Tz,
    pub log_level: String,
    pub graphql_url: String,
}

impl Config {
    pub fn load() -> Result<Config, ConfigError> {
        let document = document_from_env(&|name| std::env::var(name).ok())
            .map_err(|problems| ConfigError { problems })?;
        Config::from_document(&document, &|path| std::fs::read_to_string(path))
    }

    pub fn from_document(
        document: &Value,
        read_file: &dyn Fn(&str) -> io::Result<String>,
    ) -> Result<Config, ConfigError> {
        build(document, read_file).map_err(|problems| ConfigError { problems })
    }
}

pub fn validate_settings() -> Vec<Problem> {
    let (document, mut problems) = partial_document_from_env(&|name| std::env::var(name).ok());
    let known: Vec<String> = problems.iter().map(|p| p.path.clone()).collect();
    problems.extend(
        validate(&document)
            .into_iter()
            .filter(|p| !known.contains(&p.path)),
    );
    problems
}

fn validate(document: &Value) -> Vec<Problem> {
    // The validator never touches the disk, so a file field reads as a placeholder.
    let placeholder = |_: &str| Ok("x".to_owned());
    build(document, &placeholder).err().unwrap_or_default()
}

pub fn document_from_env(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Value, Vec<Problem>> {
    let (document, problems) = partial_document_from_env(lookup);
    if problems.is_empty() {
        return Ok(document);
    }
    Err(problems)
}

fn partial_document_from_env(lookup: &dyn Fn(&str) -> Option<String>) -> (Value, Vec<Problem>) {
    let mut document = Map::new();
    let mut problems = Vec::new();
    for field in declared_fields() {
        let name = text_of(&field, "name");
        let env = text_of(&field, "env");
        let value = lookup(&env).filter(|v| !v.is_empty());
        let Some(text) = value else {
            if let Some(default) = field.get("default") {
                document.insert(name, default.clone());
            }
            continue;
        };
        match convert(&field, &text) {
            Ok(converted) => {
                document.insert(name, converted);
            }
            Err(bad) => {
                let shown = if field.get("secret") == Some(&Value::Bool(true)) {
                    REDACTED.to_owned()
                } else {
                    bad.text
                };
                problems.push(Problem::new(
                    &name,
                    format!("{env} is not {} (\"{shown}\")", bad.kind),
                ));
            }
        }
    }
    (Value::Object(document), problems)
}

struct BadText {
    kind: String,
    text: String,
}

fn declared_fields() -> Vec<Value> {
    let declaration: Value =
        serde_json::from_str(SETTINGS_JSON).expect("settings.json is valid JSON");
    match declaration.get("fields") {
        Some(Value::Array(fields)) => fields.clone(),
        _ => Vec::new(),
    }
}

fn text_of(field: &Value, key: &str) -> String {
    field
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn convert(field: &Value, text: &str) -> Result<Value, BadText> {
    let kind = text_of(field, "type");
    if kind != "list" {
        return convert_one(&kind, text);
    }
    let item_kind = field
        .get("items")
        .map(|items| text_of(items, "type"))
        .unwrap_or_default();
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| convert_one(&item_kind, item))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn convert_one(kind: &str, text: &str) -> Result<Value, BadText> {
    let converted = match kind {
        "integer" => text.parse::<i64>().ok().map(Value::from),
        "boolean" => bool::from_str(text).ok().map(Value::from),
        _ => Some(Value::from(text)),
    };
    converted.ok_or_else(|| BadText {
        kind: kind.to_owned(),
        text: text.to_owned(),
    })
}

fn build(
    document: &Value,
    read_file: &dyn Fn(&str) -> io::Result<String>,
) -> Result<Config, Vec<Problem>> {
    let type_problems = type_problems(document);
    if !type_problems.is_empty() {
        return Err(type_problems);
    }
    let raw: RawConfig = serde_json::from_value(document.clone())
        .map_err(|err| vec![Problem::new("", err.to_string())])?;
    let mut problems = Vec::new();
    let database_url = secret_or_file(
        "database_url",
        &raw.database_url,
        "database_url_file",
        &raw.database_url_file,
        read_file,
        &mut problems,
    );
    let password = secret_or_file(
        "ava_password",
        &raw.ava_password,
        "ava_password_file",
        &raw.ava_password_file,
        read_file,
        &mut problems,
    );
    let username = username(&raw.ava_username, &mut problems);
    if raw.courses.is_empty() {
        problems.push(Problem::new("courses", "at least one course is required"));
    }
    let check_interval = duration_at_least(
        "check_interval",
        &raw.check_interval,
        MINIMUM_CHECK_INTERVAL,
        &mut problems,
    );
    let failing_alert_after = duration_at_least(
        "failing_alert_after",
        &raw.failing_alert_after,
        MINIMUM_FAILING_ALERT_AFTER,
        &mut problems,
    );
    let payment_overdue_repeat = overdue_repeat(&raw.payment_overdue_repeat, &mut problems);
    let payment_reminder_days = reminder_days(
        "payment_reminder_days",
        &raw.payment_reminder_days,
        &mut problems,
    );
    let deadline_reminder_days = reminder_days(
        "deadline_reminder_days",
        &raw.deadline_reminder_days,
        &mut problems,
    );
    let billing_day_net = billing_day_net(&raw.billing_day_net, &mut problems);
    let reminder_hour = reminder_hour(raw.reminder_hour, &mut problems);
    let timezone = timezone(&raw.ava_timezone, &mut problems);
    let boleto_link = required("boleto_link", raw.boleto_link, &mut problems);
    let log_level = required("log_level", raw.log_level.clone(), &mut problems);
    let graphql_url = required(
        "ava_graphql_url",
        raw.ava_graphql_url.clone(),
        &mut problems,
    );
    problems.extend(choice_problems(document));

    let (
        Some(database_url),
        Some(username),
        Some(password),
        Some(check_interval),
        Some(payment_reminder_days),
        Some(payment_overdue_repeat),
        Some(boleto_link),
        Some(billing_day_net),
        Some(deadline_reminder_days),
        Some(reminder_hour),
        Some(failing_alert_after),
        Some(timezone),
        Some(log_level),
        Some(graphql_url),
    ) = (
        database_url,
        username,
        password,
        check_interval,
        payment_reminder_days,
        payment_overdue_repeat,
        boleto_link,
        billing_day_net,
        deadline_reminder_days,
        reminder_hour,
        failing_alert_after,
        timezone,
        log_level,
        graphql_url,
    )
    else {
        return Err(problems);
    };
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Config {
        database_url,
        username,
        password,
        courses: raw.courses,
        check_interval,
        payment_reminder_days,
        payment_overdue_repeat,
        boleto_link,
        billing_day_net,
        deadline_reminder_days,
        calendar_categories: raw.calendar_categories,
        reminder_hour,
        failing_alert_after,
        timezone,
        log_level,
        graphql_url,
    })
}

// Runs before serde, whose errors quote the refused value: this names the path only.
fn type_problems(document: &Value) -> Vec<Problem> {
    declared_fields()
        .iter()
        .filter_map(|field| {
            let name = text_of(field, "name");
            let value = document.get(&name).filter(|v| !v.is_null())?;
            let kind = declared_kind(field);
            if has_kind(&kind, value) {
                return None;
            }
            Some(Problem::new(&name, format!("must be {kind}")))
        })
        .collect()
}

fn choice_problems(document: &Value) -> Vec<Problem> {
    declared_fields()
        .iter()
        .filter_map(|field| {
            let name = text_of(field, "name");
            let (options, rule) = declared_choices(field)?;
            let value = document.get(&name).filter(|v| !v.is_null())?;
            let values = value
                .as_array()
                .map_or_else(|| vec![value], |items| items.iter().collect());
            let allowed = |v: &&Value| v.as_str().is_some_and(|text| options.contains(&text));
            if values.iter().all(allowed) {
                return None;
            }
            Some(Problem::new(
                &name,
                format!("{rule} be one of {}", options.join(", ")),
            ))
        })
        .collect()
}

fn declared_choices(field: &Value) -> Option<(Vec<&str>, &'static str)> {
    let (options, rule) = match field.get("options") {
        Some(options) => (options, "must"),
        None => (field.get("items")?.get("options")?, "each item must"),
    };
    let options = options
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    Some((options, rule))
}

fn declared_kind(field: &Value) -> String {
    let kind = text_of(field, "type");
    if kind != "list" {
        return kind;
    }
    let item = field
        .get("items")
        .map(|i| text_of(i, "type"))
        .unwrap_or_default();
    format!("list of {item}")
}

fn has_kind(kind: &str, value: &Value) -> bool {
    if let Some(item) = kind.strip_prefix("list of ") {
        return value
            .as_array()
            .is_some_and(|items| items.iter().all(|i| has_kind(item, i)));
    }
    match kind {
        "integer" => value.is_i64(),
        "boolean" => value.is_boolean(),
        _ => value.is_string(),
    }
}

fn required<T>(path: &str, value: Option<T>, problems: &mut Vec<Problem>) -> Option<T> {
    if value.is_none() {
        problems.push(Problem::new(path, "is required"));
    }
    value
}

fn secret_or_file(
    path: &str,
    value: &Option<String>,
    file_path: &str,
    file: &Option<String>,
    read_file: &dyn Fn(&str) -> io::Result<String>,
    problems: &mut Vec<Problem>,
) -> Option<Secret> {
    let value = value.as_deref().filter(|v| !v.is_empty());
    let file = file.as_deref().filter(|f| !f.is_empty());
    let (value, file) = match (value, file) {
        (Some(_), Some(_)) => {
            problems.push(Problem::new(
                path,
                format!("set {path} or {file_path}, not both"),
            ));
            return None;
        }
        (None, None) => {
            problems.push(Problem::new(
                path,
                format!("{path} or {file_path} is required"),
            ));
            return None;
        }
        pair => pair,
    };
    if let Some(value) = value {
        return Some(Secret::new(value.to_owned()));
    }
    let content = match file.map(read_file) {
        Some(Ok(content)) => content,
        _ => {
            problems.push(Problem::new(file_path, "the file cannot be read"));
            return None;
        }
    };
    let content = content.strip_suffix('\n').unwrap_or(&content);
    if content.is_empty() {
        problems.push(Problem::new(file_path, "the file is empty"));
        return None;
    }
    Some(Secret::new(content.to_owned()))
}

fn username(value: &Option<String>, problems: &mut Vec<Problem>) -> Option<Secret> {
    let Some(value) = value.as_deref().filter(|v| !v.is_empty()) else {
        problems.push(Problem::new("ava_username", "is required"));
        return None;
    };
    let digits: String = value
        .chars()
        .filter(|c| !matches!(c, '.' | '-' | ' '))
        .collect();
    if digits.len() == USERNAME_DIGITS && digits.chars().all(|c| c.is_ascii_digit()) {
        return Some(Secret::new(digits));
    }
    problems.push(Problem::new(
        "ava_username",
        format!("must have {USERNAME_DIGITS} digits once dots, dashes and spaces are dropped"),
    ));
    None
}

fn parse_duration(text: &str) -> Option<Duration> {
    if text == "0" {
        return Some(Duration::ZERO);
    }
    let units = [("s", 1u64), ("m", 60), ("h", 3600)];
    units.iter().find_map(|(suffix, seconds)| {
        let amount: u64 = text.strip_suffix(suffix)?.parse().ok()?;
        Some(Duration::from_secs(amount.checked_mul(*seconds)?))
    })
}

fn duration_at(
    path: &str,
    value: &Option<String>,
    problems: &mut Vec<Problem>,
) -> Option<Duration> {
    let text = required(path, value.as_deref(), problems)?;
    let parsed = parse_duration(text);
    if parsed.is_none() {
        problems.push(Problem::new(
            path,
            "must be 0 or a whole number followed by s, m or h",
        ));
    }
    parsed
}

fn duration_at_least(
    path: &str,
    value: &Option<String>,
    minimum: Duration,
    problems: &mut Vec<Problem>,
) -> Option<Duration> {
    let parsed = duration_at(path, value, problems)?;
    if parsed >= minimum {
        return Some(parsed);
    }
    problems.push(Problem::new(
        path,
        format!("must be at least {}m", minimum.as_secs() / 60),
    ));
    None
}

fn overdue_repeat(value: &Option<String>, problems: &mut Vec<Problem>) -> Option<Option<Duration>> {
    let parsed = duration_at("payment_overdue_repeat", value, problems)?;
    Some(Some(parsed).filter(|d| !d.is_zero()))
}

fn reminder_days(path: &str, days: &[i64], problems: &mut Vec<Problem>) -> Option<Vec<u32>> {
    let valid = days.iter().all(|d| (0..=MAXIMUM_REMINDER_DAY).contains(d));
    if !valid {
        problems.push(Problem::new(
            path,
            format!("each day must be from 0 to {MAXIMUM_REMINDER_DAY}"),
        ));
        return None;
    }
    let mut days: Vec<u32> = days.iter().filter_map(|d| u32::try_from(*d).ok()).collect();
    days.sort_unstable_by(|a, b| b.cmp(a));
    days.dedup();
    Some(days)
}

fn billing_day_net(value: &Option<String>, problems: &mut Vec<Problem>) -> Option<BillingDayNet> {
    let text = required("billing_day_net", value.as_deref(), problems)?;
    let parsed = match text {
        "auto" => Some(BillingDayNet::Auto),
        "off" => Some(BillingDayNet::Off),
        day => day
            .parse::<u32>()
            .ok()
            .filter(|d| (1..=28).contains(d))
            .map(BillingDayNet::Fixed),
    };
    if parsed.is_none() {
        problems.push(Problem::new(
            "billing_day_net",
            "must be auto, off or a day from 1 to 28",
        ));
    }
    parsed
}

fn reminder_hour(value: Option<i64>, problems: &mut Vec<Problem>) -> Option<u32> {
    let hour = required("reminder_hour", value, problems)?;
    let parsed = u32::try_from(hour).ok().filter(|h| *h <= 23);
    if parsed.is_none() {
        problems.push(Problem::new("reminder_hour", "must be from 0 to 23"));
    }
    parsed
}

fn timezone(value: &Option<String>, problems: &mut Vec<Problem>) -> Option<chrono_tz::Tz> {
    let name = required("ava_timezone", value.as_deref(), problems)?;
    let parsed = chrono_tz::Tz::from_str(name).ok();
    if parsed.is_none() {
        problems.push(Problem::new(
            "ava_timezone",
            "must be an IANA time zone name",
        ));
    }
    parsed
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const BASE: [(&str, &str); 4] = [
        ("DATABASE_URL", "postgres://x"),
        ("AVA_USERNAME", "12345678909"),
        ("AVA_PASSWORD", "x"),
        ("COURSES", "A"),
    ];

    fn document(pairs: &[(&str, &str)]) -> Result<Value, Vec<Problem>> {
        document_from_env(&|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    fn with(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = BASE
            .iter()
            .filter(|(k, _)| !extra.iter().any(|(e, _)| e == k))
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        pairs.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        pairs
    }

    fn load(pairs: &[(String, String)]) -> Result<Config, Vec<Problem>> {
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let document = document(&borrowed)?;
        Config::from_document(&document, &|_| Ok("file-content\n".to_owned()))
            .map_err(|err| err.problems)
    }

    fn problems_of(pairs: &[(String, String)]) -> Vec<Problem> {
        load(pairs).err().unwrap_or_default()
    }

    fn paths(problems: &[Problem]) -> Vec<&str> {
        problems.iter().map(|p| p.path.as_str()).collect()
    }

    #[test]
    fn a_general_problem_is_shown_without_a_path() {
        let error = ConfigError {
            problems: vec![
                Problem::new("", "the configuration document is not a JSON object"),
                Problem::new("courses", "at least one course is required"),
            ],
        };

        assert_eq!(
            error.to_string(),
            "the configuration document is not a JSON object; courses: at least one course is required"
        );
    }

    #[test]
    fn defaults_come_from_the_declaration() {
        let config = load(&with(&[])).expect("valid configuration");

        assert_eq!(config.check_interval, Duration::from_secs(3600));
        assert_eq!(config.payment_reminder_days, vec![7, 3, 1, 0]);
        assert_eq!(
            config.payment_overdue_repeat,
            Some(Duration::from_secs(86400))
        );
        assert!(config.boleto_link);
        assert_eq!(config.billing_day_net, BillingDayNet::Auto);
        assert_eq!(config.deadline_reminder_days, vec![7, 3, 1]);
        assert_eq!(
            config.calendar_categories,
            vec!["FINANCIAL", "SECRETARIAT", "UNIQUE_EVENTS"]
        );
        assert_eq!(config.reminder_hour, 9);
        assert_eq!(config.failing_alert_after, Duration::from_secs(86400));
        assert_eq!(config.timezone, chrono_tz::America::Sao_Paulo);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.graphql_url, "https://graphql.ampli.com.br/");
        assert_eq!(config.courses, vec!["A"]);
        assert_eq!(config.database_url.reveal(), "postgres://x");
    }

    #[test]
    fn username_keeps_digits_only() {
        let config =
            load(&with(&[("AVA_USERNAME", "123.456.789-09")])).expect("valid configuration");
        assert_eq!(config.username.reveal(), "12345678909");

        let problems = problems_of(&with(&[("AVA_USERNAME", "1234")]));
        assert_eq!(paths(&problems), vec!["ava_username"]);
    }

    #[test]
    fn every_problem_is_reported_at_once_with_paths() {
        let problems = document(&[])
            .and_then(|d| Config::from_document(&d, &|_| Ok(String::new())).map_err(|e| e.problems))
            .err()
            .unwrap_or_default();

        assert_eq!(
            paths(&problems),
            vec!["database_url", "ava_password", "ava_username", "courses"]
        );
    }

    #[test]
    fn password_and_password_file_are_exclusive() {
        let problems = problems_of(&with(&[("AVA_PASSWORD_FILE", "/run/secret")]));
        assert_eq!(paths(&problems), vec!["ava_password"]);

        let from_file = load(&with(&[
            ("AVA_PASSWORD", ""),
            ("AVA_PASSWORD_FILE", "/run/secret"),
        ]))
        .expect("valid configuration");
        assert_eq!(from_file.password.reveal(), "file-content");
    }

    #[test]
    fn lists_split_on_commas_and_convert_by_type() {
        let config =
            load(&with(&[("PAYMENT_REMINDER_DAYS", "1, 7,3,3")])).expect("valid configuration");
        assert_eq!(config.payment_reminder_days, vec![7, 3, 1]);

        let problem = document(&[("PAYMENT_REMINDER_DAYS", "x")]).expect_err("a problem");
        assert_eq!(
            problem,
            vec![Problem::new(
                "payment_reminder_days",
                "PAYMENT_REMINDER_DAYS is not integer (\"x\")"
            )]
        );
    }

    #[test]
    fn boolean_text_must_be_true_or_false() {
        let config = load(&with(&[("BOLETO_LINK", "false")])).expect("valid configuration");
        assert!(!config.boleto_link);

        let problem = document(&[("BOLETO_LINK", "on")]).expect_err("a problem");
        assert_eq!(paths(&problem), vec!["boleto_link"]);
    }

    #[test]
    fn billing_day_net_accepts_auto_off_and_days() {
        let read = |value: &str| {
            load(&with(&[("BILLING_DAY_NET", value)]))
                .ok()
                .map(|c| c.billing_day_net)
        };

        assert_eq!(read("auto"), Some(BillingDayNet::Auto));
        assert_eq!(read("off"), Some(BillingDayNet::Off));
        assert_eq!(read("1"), Some(BillingDayNet::Fixed(1)));
        assert_eq!(read("28"), Some(BillingDayNet::Fixed(28)));
        assert_eq!(read("0"), None);
        assert_eq!(read("29"), None);
    }

    #[test]
    fn choices_must_be_declared_options() {
        let config = load(&with(&[
            ("LOG_LEVEL", "debug"),
            ("CALENDAR_CATEGORIES", "SECRETARIAT, UNIQUE_EVENTS"),
        ]))
        .expect("valid configuration");
        assert_eq!(config.log_level, "debug");
        assert_eq!(
            config.calendar_categories,
            vec!["SECRETARIAT", "UNIQUE_EVENTS"]
        );

        let problems = problems_of(&with(&[
            ("LOG_LEVEL", "verbose"),
            ("CALENDAR_CATEGORIES", "FINANCIAL,HOLIDAYS"),
        ]));
        assert_eq!(
            problems,
            vec![
                Problem::new(
                    "calendar_categories",
                    "each item must be one of FINANCIAL, SECRETARIAT, UNIQUE_EVENTS"
                ),
                Problem::new(
                    "log_level",
                    "must be one of trace, debug, info, warn, error"
                ),
            ]
        );

        let mut document = full_document();
        document.remove("database_url_file");
        document.remove("ava_password_file");
        document.insert("log_level".to_owned(), Value::from("INFO"));
        assert_eq!(
            paths(&validate(&Value::Object(document))),
            vec!["log_level"]
        );
    }

    #[test]
    fn overdue_repeat_zero_disables_repetition() {
        let config = load(&with(&[("PAYMENT_OVERDUE_REPEAT", "0")])).expect("valid configuration");
        assert_eq!(config.payment_overdue_repeat, None);
    }

    #[test]
    fn durations_and_ranges_are_checked() {
        let cases = [
            ("CHECK_INTERVAL", "14m", "check_interval"),
            ("CHECK_INTERVAL", "1d", "check_interval"),
            ("FAILING_ALERT_AFTER", "59m", "failing_alert_after"),
            ("REMINDER_HOUR", "24", "reminder_hour"),
            ("PAYMENT_REMINDER_DAYS", "61", "payment_reminder_days"),
            ("AVA_TIMEZONE", "Mars/Olympus", "ava_timezone"),
        ];
        for (name, value, path) in cases {
            let problems = problems_of(&with(&[(name, value)]));
            assert_eq!(paths(&problems), vec![path], "{name}={value}");
        }
        let config = load(&with(&[("CHECK_INTERVAL", "900s")])).expect("valid configuration");
        assert_eq!(config.check_interval, Duration::from_secs(900));
    }

    #[test]
    fn secrets_never_reach_problem_text() {
        let pairs = with(&[("AVA_USERNAME", "abc-secret-text"), ("REMINDER_HOUR", "99")]);
        let problems = problems_of(&pairs);

        assert_eq!(paths(&problems), vec!["ava_username", "reminder_hour"]);
        assert!(
            problems
                .iter()
                .all(|p| !p.message.contains("abc-secret-text"))
        );
    }

    fn valid_value(name: &str) -> Value {
        let values = [
            ("database_url", Value::from("postgres://x")),
            ("database_url_file", Value::from("/run/db")),
            ("ava_username", Value::from("12345678909")),
            ("ava_password", Value::from("x")),
            ("ava_password_file", Value::from("/run/pw")),
            ("courses", Value::from(vec!["A"])),
            ("check_interval", Value::from("1h")),
            ("payment_reminder_days", Value::from(vec![7, 3])),
            ("payment_overdue_repeat", Value::from("24h")),
            ("boleto_link", Value::from(true)),
            ("billing_day_net", Value::from("auto")),
            ("deadline_reminder_days", Value::from(vec![1])),
            ("calendar_categories", Value::from(vec!["FINANCIAL"])),
            ("reminder_hour", Value::from(9)),
            ("failing_alert_after", Value::from("24h")),
            ("ava_timezone", Value::from("America/Sao_Paulo")),
            ("log_level", Value::from("info")),
            ("ava_graphql_url", Value::from("https://example.test/")),
        ];
        values
            .into_iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
            .unwrap_or(Value::Null)
    }

    fn full_document() -> Map<String, Value> {
        declared_fields()
            .iter()
            .map(|field| {
                let name = text_of(field, "name");
                let value = valid_value(&name);
                (name, value)
            })
            .collect()
    }

    #[test]
    fn a_document_with_every_declared_field_is_read() {
        let document = Value::Object(full_document());

        assert!(full_document().values().all(|v| !v.is_null()));
        assert!(serde_json::from_value::<RawConfig>(document.clone()).is_ok());

        let mut exclusive = full_document();
        exclusive.remove("database_url_file");
        exclusive.remove("ava_password_file");
        assert!(Config::from_document(&Value::Object(exclusive), &|_| Ok(String::new())).is_ok());
    }

    #[test]
    fn a_wrong_typed_field_is_reported_by_path_without_its_value() {
        let cases = [
            ("ava_password", serde_json::json!(5)),
            ("database_url", serde_json::json!(["leaky-secret"])),
            ("ava_username", serde_json::json!(true)),
            ("reminder_hour", serde_json::json!("leaky-secret")),
        ];
        for (name, value) in cases {
            let mut document = full_document();
            document.remove("database_url_file");
            document.remove("ava_password_file");
            document.insert(name.to_owned(), value);

            let problems = validate(&Value::Object(document));

            assert_eq!(paths(&problems), vec![name]);
            assert!(!problems[0].message.contains("leaky-secret"));
            assert!(!problems[0].message.contains('5'));
        }
    }

    #[test]
    fn a_conversion_problem_does_not_hide_the_other_problems() {
        let (document, mut problems) =
            partial_document_from_env(&|n| (n == "REMINDER_HOUR").then(|| "x".to_owned()));
        problems.extend(validate(&document));

        let found = paths(&problems);
        assert!(found.contains(&"reminder_hour"));
        assert!(found.contains(&"courses"));
    }

    #[test]
    fn an_undeclared_field_is_refused() {
        let mut document = full_document();
        document.remove("database_url_file");
        document.remove("ava_password_file");
        document.insert("unknown_field".to_owned(), Value::from(1));

        assert!(Config::from_document(&Value::Object(document), &|_| Ok(String::new())).is_err());
    }

    #[test]
    fn the_declaration_is_version_2_and_well_formed() {
        let declaration: Value = serde_json::from_str(SETTINGS_JSON).expect("valid JSON");
        assert_eq!(declaration["settings_version"], 2);

        let valid = |text: &str, first: fn(char) -> bool| {
            text.chars().next().is_some_and(first)
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '_' || first(c))
        };
        let mut names = BTreeSet::new();
        for field in declared_fields() {
            let name = text_of(&field, "name");
            let env = text_of(&field, "env");
            assert!(valid(&name, |c| c.is_ascii_lowercase()), "{name}");
            assert!(valid(&env, |c| c.is_ascii_uppercase()), "{env}");
            assert!(names.insert(name.clone()), "duplicate {name}");
            if field.get("editable") == Some(&Value::Bool(false)) {
                assert!(!text_of(&field, "reason").is_empty(), "{name}");
            }
            if field.get("secret") == Some(&Value::Bool(true)) {
                assert!(field.get("default").is_none(), "{name}");
            }
        }
    }
}
