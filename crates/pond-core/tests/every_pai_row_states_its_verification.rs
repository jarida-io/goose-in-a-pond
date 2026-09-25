//! Every PAI checklist row must say whether the capability was RUN, not only whether it landed.
//! This checks the claim is made, not that it's true; `scripts/pai-bench.sh` is for that.

const CHECKLIST: &str = include_str!("../../../docs/architecture/pai/00-checklist.md");

/// Section 1's requirement rows, by the PAI document each links to.
const PAI_DOCS: &[&str] = &[
    "01-identity",
    "02-privacy",
    "03-context",
    "04-smart",
    "05-reasoning",
    "06-multi",
    "07-proactive",
    "08-personal",
];

/// One of these must appear in a row's status cell.
const VERIFICATION_WORDS: &[&str] = &["VERIFIED", "UNVERIFIABLE"];

fn requirement_rows() -> Vec<(&'static str, &'static str)> {
    let mut rows = Vec::new();
    for doc in PAI_DOCS {
        if let Some(line) = CHECKLIST
            .lines()
            .find(|l| l.starts_with("| ") && l.contains("](./0") && l.contains(doc))
        {
            rows.push((*doc, line));
        }
    }
    rows
}

/// Without this, a moved file or reshaped table makes every test below pass vacuously.
#[test]
fn the_file_this_test_reads_is_the_checklist_and_it_still_has_a_table() {
    assert!(
        CHECKLIST.contains("# PAI working checklist"),
        "CHECKLIST is not 00-checklist.md"
    );
    let rows = requirement_rows();
    assert_eq!(
        rows.len(),
        PAI_DOCS.len(),
        "found {} of {} requirement rows in section 1. The table has been reshaped or a \
         workstream's document was renamed, and this guard is now reading nothing: {:?}",
        rows.len(),
        PAI_DOCS.len(),
        rows.iter().map(|(d, _)| *d).collect::<Vec<_>>()
    );
}

#[test]
fn every_requirement_row_states_whether_it_has_been_run() {
    let silent: Vec<&str> = requirement_rows()
        .into_iter()
        .filter(|(_, line)| !VERIFICATION_WORDS.iter().any(|w| line.contains(w)))
        .map(|(doc, _)| doc)
        .collect();

    assert!(
        silent.is_empty(),
        "these PAI rows say whether the code landed and not whether the capability WORKS: \
         {silent:?}.\n\
         Add one of {VERIFICATION_WORDS:?} to the status cell in \
         docs/architecture/pai/00-checklist.md section 1, per the vocabulary table just below \
         it. `NOT VERIFIED` is a perfectly good answer and PAI-7 currently carries it -- what is \
         not allowed is saying nothing, because that is indistinguishable from working and it is \
         how three capabilities came to be recorded as finished while broken.\n\
         Run `scripts/pai-bench.sh` (add --slow for PAI-7) to find out which word is true."
    );
}

/// Mac and Orin differ ~30x on decode and on KV geometry: a claim must name its hardware.
#[test]
fn a_verified_row_names_the_hardware_and_the_date() {
    let vague: Vec<&str> = requirement_rows()
        .into_iter()
        .filter(|(_, line)| line.contains("VERIFIED"))
        .filter(|(_, line)| {
            !(line.contains("Orin") || line.contains("Jetson") || line.contains("Mac"))
        })
        .map(|(doc, _)| doc)
        .collect();

    assert!(
        vague.is_empty(),
        "these rows claim a verification without naming the hardware it ran on: {vague:?}. \
         The Mac and the Orin differ by ~30x on decode and disagree about KV geometry, so an \
         unattributed number cannot be repeated or compared."
    );
}
