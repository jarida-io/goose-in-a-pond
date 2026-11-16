//! The don't-list as makers and critics read it, and the maker that turns "never this" into one
//! general rule. Nothing here calls a model.

use crate::user_data::domain::dont_list::{clean_rule, rule_key, DontRule, Reason, RuleOrigin};
use crate::user_data::services::model_json::{object_in, text_of};

/// `avoid` rules one brief carries, the most relevant first.
pub const AVOID_RULES_PER_BRIEF: usize = 10;
/// Longest rule the maker may write; a longer one is a retelling, not a rule.
pub const MAX_MADE_RULE_CHARS: usize = 120;
const MAX_SAID_CHARS: usize = 400;
const MAX_HOOK_CHARS: usize = 240;

fn brief_line(rule: &DontRule) -> String {
    let mut line = rule.as_line();
    if rule.origin == RuleOrigin::Fact {
        line.push_str(" (they took this off their fact sheet)");
    }
    if let Some(said) = rule
        .said
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        line.push_str(&format!(". They said: \"{said}\""));
    }
    line
}

/// The don't-list a brief carries, a rule to a line: every `never` rule, then at most
/// [`AVOID_RULES_PER_BRIEF`] of `relevant_avoid`, which the caller orders by relevance. Empty
/// when the member has set none.
pub fn for_brief(never: &[DontRule], relevant_avoid: &[DontRule]) -> String {
    never
        .iter()
        .chain(relevant_avoid.iter().take(AVOID_RULES_PER_BRIEF))
        .map(brief_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The rule maker's system prompt, fixed so it stays in the model's cached prefix.
pub const RULE_SYSTEM: &str = "\
A member of this household turned down something the pond made for them and said \"never \
this\". Write the one general rule the pond follows from now on, so it never makes that \
kind of thing for them again.

You are given what was turned down (its title, what kind of thing it was and its opening \
line), the reasons they picked, and their own words.

Reply with JSON and nothing else:

{\"rule\": \"<rule>\"}

- The rule names the kind of thing to stop, not this one item: wide enough to cover the next \
one like it, narrow enough to leave alone what they did not object to.
- Build it on their own words first, then on the reasons they picked.
- One short phrase under 100 characters. Do not start it with \"never\" or \"avoid\", and do \
not end it with a full stop.
- Do not repeat the title.
- {\"rule\": \"\"} when their words and reasons do not say what kind of thing to stop.
No explanation, no preamble.";

fn one_line(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// The rule maker's message. `kind` is what the thing was, in plain words (a shortlist, a
/// plan); `hook` is its opening line; `said` is the member's own words.
pub fn build_rule_message(
    title: &str,
    kind: &str,
    hook: &str,
    reasons: &[Reason],
    said: Option<&str>,
) -> String {
    let mut out = format!("Turned down: {}\n", one_line(title, MAX_HOOK_CHARS));
    let kind = one_line(kind, MAX_HOOK_CHARS);
    if !kind.is_empty() {
        out.push_str(&format!("Kind: {kind}\n"));
    }
    let hook = one_line(hook, MAX_HOOK_CHARS);
    if !hook.is_empty() {
        out.push_str(&format!("Opening line: {hook}\n"));
    }
    if !reasons.is_empty() {
        let picked: Vec<&str> = reasons.iter().map(|r| r.as_words()).collect();
        out.push_str(&format!("Reasons they picked: {}\n", picked.join("; ")));
    }
    let said = said
        .map(|s| one_line(s, MAX_SAID_CHARS))
        .unwrap_or_default();
    if said.is_empty() {
        out.push_str("Their words: none\n");
    } else {
        out.push_str(&format!("Their words: \"{said}\"\n"));
    }
    out.push_str("\nJSON:");
    out
}

/// The rule the maker wrote, as it is stored. `None` when the reply holds none, the rule is
/// longer than [`MAX_MADE_RULE_CHARS`], or it only names `title` again: the caller then keeps
/// the member's own words.
pub fn parse_rule(raw: &str, title: &str) -> Option<String> {
    let map = object_in(raw)?;
    let rule = clean_rule(&text_of(&map, "rule")?)?;
    if rule.chars().count() > MAX_MADE_RULE_CHARS {
        return None;
    }
    let (key, title_key) = (rule_key(&rule), rule_key(title));
    if !title_key.is_empty() && key.contains(&title_key) {
        return None;
    }
    Some(rule.trim_end_matches('.').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::dont_list::Strength;
    use chrono::Utc;

    fn never(rule: &str) -> DontRule {
        DontRule::new("jerry", rule, Strength::Never, RuleOrigin::You, Utc::now()).unwrap()
    }

    fn avoid(title: &str, reasons: &[Reason]) -> DontRule {
        DontRule::avoid_title(
            "jerry",
            title,
            reasons,
            RuleOrigin::Reject,
            "d1",
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn a_brief_carries_every_never_rule_and_ten_avoid_rules_at_most() {
        let nevers: Vec<DontRule> = (0..14).map(|i| never(&format!("rule {i}"))).collect();
        let avoids: Vec<DontRule> = (0..14).map(|i| avoid(&format!("Title {i}"), &[])).collect();
        let brief = for_brief(&nevers, &avoids);
        assert_eq!(brief.matches("Never: rule ").count(), 14);
        assert_eq!(
            brief.matches("Avoid: Title ").count(),
            AVOID_RULES_PER_BRIEF
        );
        assert!(
            brief.ends_with("Avoid: Title 9"),
            "the caller's order decides"
        );
        assert!(brief.find("Never: rule 13").unwrap() < brief.find("Avoid: Title 0").unwrap());
    }

    #[test]
    fn a_rule_reads_as_one_line_with_why_it_was_set() {
        let mut said = avoid(
            "A duck recipe in teal",
            &[Reason::TooGeneric, Reason::WrongFacts],
        );
        said.said = Some(" I do not cook ".into());
        let mut fact = avoid("Likes duck recipes", &[]);
        fact.origin = RuleOrigin::Fact;
        assert_eq!(
            for_brief(&[never("celebrity gossip")], &[said, fact]),
            "Never: celebrity gossip\n\
             Avoid: A duck recipe in teal (too generic, built on wrong facts). They said: \"I do \
             not cook\"\n\
             Avoid: Likes duck recipes (they took this off their fact sheet)"
        );
        assert_eq!(for_brief(&[], &[]), "");
    }

    #[test]
    fn the_rule_maker_is_told_what_was_turned_down_and_why() {
        let message = build_rule_message(
            "A duck recipe in teal",
            "a recipe",
            "Since you like teal,\nhere is a duck dish.",
            &[Reason::TooGeneric, Reason::NotRelevant],
            Some("stop building  on colours"),
        );
        assert_eq!(
            message,
            "Turned down: A duck recipe in teal\n\
             Kind: a recipe\n\
             Opening line: Since you like teal, here is a duck dish.\n\
             Reasons they picked: too generic; not relevant to them\n\
             Their words: \"stop building on colours\"\n\
             \n\
             JSON:"
        );
        let bare = build_rule_message("A duck recipe", "", "", &[], None);
        assert_eq!(
            bare,
            "Turned down: A duck recipe\nTheir words: none\n\nJSON:"
        );
        assert!(RULE_SYSTEM.contains("{\"rule\": \"<rule>\"}"));
        assert!(RULE_SYSTEM.contains("Do not repeat the title"));
    }

    #[test]
    fn a_made_rule_is_general_short_and_not_the_title_again() {
        let title = "A duck recipe in teal";
        assert_eq!(
            parse_rule(
                "```json\n{\"rule\": \"Never: recipes built on a colour preference.\"}\n```",
                title
            )
            .as_deref(),
            Some("recipes built on a colour preference")
        );
        assert_eq!(
            parse_rule(r#"{"rule": "duck recipes"}"#, title).as_deref(),
            Some("duck recipes"),
            "narrower than the title is still a rule"
        );
        for refused in [
            r#"{"rule": "A duck recipe in teal"}"#,
            r#"{"rule": "never offer a duck recipe in teal again"}"#,
            r#"{"rule": ""}"#,
            r#"{"rule": "<rule>"}"#,
            "I would suggest avoiding recipes.",
        ] {
            assert_eq!(parse_rule(refused, title), None, "{refused}");
        }
        let long = format!(r#"{{"rule": "{}"}}"#, "recipes and ".repeat(20));
        assert_eq!(parse_rule(&long, title), None, "a retelling is not a rule");
    }
}
