//! Community (parent group) actions on the WhatsApp send tool: creating and
//! deactivating a community, linking and unlinking its subgroups, joining one,
//! and reading its structure and membership.
//!
//! The arms need a live socket, so the wire calls cannot run here. What is
//! checkable without a connection is the part that decides what the model can
//! reach and which call each action makes: the dispatch table, and the rules
//! the resolvers and formatters enforce.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");

/// Every action this task added, paired with the call it must make.
///
/// The receiver is part of the pin: a community read answered through the
/// group surface would compile and quietly return the wrong thing, because a
/// community is also a group.
const ACTIONS: &[(&str, &str)] = &[
    ("community_create", "community().create("),
    ("community_deactivate", "community().deactivate("),
    ("community_create_subgroup", "community().create_subgroup("),
    ("community_link_subgroups", "community().link_subgroups("),
    (
        "community_unlink_subgroups",
        "community().unlink_subgroups(",
    ),
    ("community_join_subgroup", "community().join_subgroup("),
    (
        "community_remove_participants",
        "community().remove_participants(",
    ),
    (
        "community_get_participating",
        "community().get_participating(",
    ),
    ("community_get_subgroups", "community().get_subgroups("),
    (
        "community_get_subgroup_participant_counts",
        "community().get_subgroup_participant_counts(",
    ),
    (
        "community_query_linked_group",
        "community().query_linked_group(",
    ),
    (
        "community_get_linked_groups_participants",
        "community().get_linked_groups_participants(",
    ),
];

/// The actions that only report what the server already knows.
const READERS: &[&str] = &[
    "community_get_participating",
    "community_get_subgroups",
    "community_get_subgroup_participant_counts",
    "community_query_linked_group",
    "community_get_linked_groups_participants",
];

/// The text with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The body of the match arm that handles `action`: from its pattern to the
/// start of the next arm.
///
/// Two arm shapes reach the dispatch table here, `=> {` and `=> match ... {`,
/// and both are accepted. Pins are scoped to the arm on purpose: a file-wide
/// `contains` would be satisfied by a call living anywhere in a four-thousand
/// line file, so an action whose arm was deleted while its helper stayed in use
/// elsewhere would still pass.
fn arm_body(action: &str) -> &'static str {
    let needle = format!("\"{action}\"");
    let mut from = 0;
    loop {
        let at = SOURCE[from..]
            .find(&needle)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("'{action}' does not appear in the source"));
        let after = &SOURCE[at + needle.len()..];
        // Only a match arm follows the name with ` => `; the schema enum and
        // the unknown-action list follow it with a comma.
        let is_arm = after.starts_with(" => {") || after.starts_with(" => match ");
        if is_arm {
            let ends = [
                after.find("\n            \""),
                after.find("\n            unknown"),
            ];
            let end = ends.into_iter().flatten().min().unwrap_or(after.len());
            return &after[..end];
        }
        from = at + needle.len();
    }
}

/// The body of the shared group-JID resolver, from its signature to its closing
/// brace.
fn group_resolver_body() -> &'static str {
    let at = SOURCE
        .find("fn resolve_group_jid_keyed")
        .expect("the shared group resolver exists");
    let rest = &SOURCE[at..];
    let end = rest.find("\n}\n").expect("the resolver closes");
    &rest[..end]
}

#[test]
fn every_community_action_has_a_match_arm_and_a_schema_entry() {
    let compact = squash(SOURCE);
    for (action, _) in ACTIONS {
        let occurrences = compact.matches(&format!("\"{action}\"")).count();
        assert!(
            occurrences >= 2,
            "'{action}' appears {occurrences} time(s): it needs both a schema enum entry \
             (so the model can discover it) and a match arm"
        );
    }
}

#[test]
fn the_community_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// A community is a group with `is_parent_group` set, so its JID is an ordinary
/// `@g.us` address. It therefore goes through the one group resolver rather
/// than a copy of it, and a group id can never be read as a phone number.
/// The actions that name a community, or a group inside one.
const TARGETED: &[&str] = &[
    "community_deactivate",
    "community_create_subgroup",
    "community_link_subgroups",
    "community_unlink_subgroups",
    "community_join_subgroup",
    "community_remove_participants",
    "community_get_subgroups",
    "community_get_subgroup_participant_counts",
    "community_query_linked_group",
    "community_get_linked_groups_participants",
];

/// A community is a group with `is_parent_group` set, so its JID is an ordinary
/// `@g.us` address. It therefore goes through the one group resolver rather
/// than a copy of it, and a group id can never be read as a phone number.
#[test]
fn a_community_is_resolved_as_a_group() {
    for action in TARGETED {
        let body = squash(arm_body(action));
        assert!(
            body.contains("resolve_group_jid(&input)")
                || body.contains("resolve_group_jids(&input)")
                || body.contains("resolve_subgroup_jid(&input)"),
            "'{action}' must resolve its target through the group resolvers"
        );
    }
    // Creating a community names a thing that does not exist yet, and listing
    // the ones this account is in names none at all, so neither may demand a
    // group JID: a required parameter with nothing to name is a dead end.
    for action in ["community_create", "community_get_participating"] {
        assert!(
            !squash(arm_body(action)).contains("resolve_group_jid(&input)"),
            "'{action}' names no existing group and must not require one"
        );
    }
}

/// A community call names two groups at once: the community and a group inside
/// it. They must not be spelled with the same parameter, or linking "A into A"
/// becomes expressible.
#[test]
fn a_subgroup_is_named_with_its_own_parameter() {
    let at = SOURCE
        .find("fn resolve_subgroup_jid")
        .expect("the subgroup resolver exists");
    let rest = &SOURCE[at..];
    let end = rest.find("\n}\n").expect("the subgroup resolver closes");
    let body = squash(&rest[..end]);
    assert!(
        body.contains("resolve_group_jid_keyed(input,\"subgroup_jid\")"),
        "the subgroup resolver must reuse the shared group resolver, keyed on 'subgroup_jid'"
    );
    for action in ["community_query_linked_group", "community_join_subgroup"] {
        assert!(
            squash(arm_body(action)).contains("resolve_subgroup_jid(&input)"),
            "'{action}' must name the group inside the community with 'subgroup_jid'"
        );
    }
}

/// A channel JID is `<id>@newsletter` and a group id is `<id>@g.us`. Accepting
/// the wrong server would point a community action at a conversation, so the
/// resolver checks the server rather than trusting the shape of the string.
#[test]
fn a_non_group_jid_is_refused() {
    let body = squash(group_resolver_body());
    assert!(
        body.contains("jid.server!=Server::Group"),
        "the resolver must reject a JID whose server is not Group"
    );
    assert!(
        body.contains("isnotagroupJID"),
        "the refusal must say what was wrong with the JID"
    );
}

/// Bare group ids are handed out by the server and appear in config, so the
/// resolver wraps one. Without this every caller would have to know the
/// `@g.us` suffix.
#[test]
fn a_bare_group_id_is_wrapped() {
    assert!(
        squash(group_resolver_body()).contains("Jid::group(raw)"),
        "a bare group id must be wrapped as a group JID"
    );
}

/// Creating, deactivating, linking, joining and removing all change who can see
/// or reach the community, so the resolver runs them past the same send scope
/// as any outbound act. The check lives in the resolver rather than in each arm
/// so a new community action cannot be added without it.
#[test]
fn the_resolver_runs_the_send_scope() {
    let body = squash(group_resolver_body());
    assert!(
        body.contains("send_scope::may_send(\"whatsapp\",&jid_str)"),
        "the resolver must consult the send scope"
    );
    assert!(
        body.contains("refusal_for(\"whatsapp\",&jid_str)"),
        "a refusal must carry the reason, not just a boolean"
    );
}

/// Two arms report the same object back: the metadata of a group inside a
/// community. Two copies of the formatter would drift into two shapes, and the
/// caller cannot tell which fields an action chose to omit.
#[test]
fn one_formatter_reports_a_linked_group() {
    for action in ["community_query_linked_group", "community_join_subgroup"] {
        assert!(
            squash(arm_body(action)).contains("describe_community_group(&meta)"),
            "'{action}' must render the metadata through the shared formatter"
        );
    }
}

/// A community reports which of its entries is the announcement group and which
/// is the general chat. Neither is distinguishable by name, so the tag has to
/// come from the flags rather than from the subject.
#[test]
fn subgroups_are_reported_with_their_kind() {
    let at = SOURCE
        .find("fn describe_community_subgroups")
        .expect("the subgroup formatter exists");
    let rest = &SOURCE[at..];
    let end = rest.find("\n}\n").expect("the formatter closes");
    let body = squash(&rest[..end]);
    for flag in ["is_default_sub_group", "is_general_chat"] {
        assert!(
            body.contains(flag),
            "the subgroup formatter must report '{flag}'"
        );
    }
    assert!(
        squash(arm_body("community_get_subgroups")).contains("describe_community_subgroups("),
        "community_get_subgroups must render the list through its formatter"
    );
}

/// The server answers a link request per group, so a partial success that named
/// only the count would read as a clean sweep and leave the caller believing
/// the missing groups are linked.
#[test]
fn a_link_reports_the_groups_the_server_refused() {
    for action in ["community_link_subgroups", "community_unlink_subgroups"] {
        assert!(
            squash(arm_body(action)).contains("describe_failed_groups(&result.failed_groups)"),
            "'{action}' must name the groups the server refused"
        );
    }
}

/// The lib's `CreateCommunityOptions::new` already defaults `closed`,
/// `allow_non_admin_sub_group_creation` and `create_general_chat`. A second
/// default invented in the arm would silently disagree with the lib the day one
/// of them changes, so an absent parameter must leave the lib's value alone.
#[test]
fn creating_a_community_keeps_the_lib_defaults() {
    let body = squash(arm_body("community_create"));
    assert!(
        body.contains("CreateCommunityOptions::new(name)"),
        "community_create must build the lib's options type"
    );
    assert!(
        !body.contains("unwrap_or("),
        "community_create must not invent a default for a parameter the lib already defaults"
    );
}

/// A community read only reports what the server already knows, so charging it
/// to the send budget would spend it on an answer that posts nothing.
#[test]
fn reading_a_community_is_not_charged_to_the_send_budget() {
    for action in READERS {
        assert!(
            !squash(arm_body(action)).contains("gate_send!"),
            "'{action}' posts nothing and must not be charged to the send budget"
        );
    }
}
