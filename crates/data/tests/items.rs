//! Item layouts must match PRD §6 exactly: attribute names, discriminators,
//! sparse GSI attributes, and numeric vote directions.

use aws_sdk_dynamodb::types::AttributeValue;
use data::Timestamp;
use data::item::{AttrMap, DynamoItem, ItemType};
use data::model::{
    Account, AppendPayload, AuthState, Collection, CoverImage, EditPayload, Event, RateLimit,
    RateLimitPartition, Report, ReportTargetType, Session, Significance, SuggestCount, Suggestion,
    SuggestionKind, SuggestionPayload, SuggestionState, Timeline, Vote, VoteDirection,
};

fn s(value: &str) -> AttributeValue {
    AttributeValue::S(value.to_owned())
}

fn n(value: &str) -> AttributeValue {
    AttributeValue::N(value.to_owned())
}

fn ts(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn sample_timeline() -> Timeline {
    Timeline {
        id: "t1".to_owned(),
        title: "A timeline".to_owned(),
        description: "About things".to_owned(),
        owner_id: "owner-1".to_owned(),
        collection_id: None,
        event_count: 2,
        upvotes: 3,
        downvotes: 1,
        created_at: ts("2026-11-07T12:30:00Z"),
        updated_at: ts("2026-11-08T09:00:00Z"),
        report_count: 0,
        flagged: false,
        published: true,
        cover_image: Some(CoverImage {
            key: "cover/t1/abc.jpg".to_owned(),
            width: 1600,
            height: 900,
            credit: Some("A photographer".to_owned()),
        }),
    }
}

fn sample_event() -> Event {
    Event {
        timeline_id: "t1".to_owned(),
        event_id: "e1".to_owned(),
        timestamp: ts("2026-11-07T12:30:00Z"),
        date: "2026-11-07".to_owned(),
        title: "An event".to_owned(),
        description: "It happened".to_owned(),
        sources: vec!["https://example.com/a".to_owned()],
        significance: Significance::Crucial,
        upvotes: 2,
        downvotes: 0,
        report_count: 0,
        flagged: false,
        created_at: ts("2026-11-07T12:31:00Z"),
        created_by: "owner-1".to_owned(),
        last_edited_by: Some("owner-2".to_owned()),
        last_edit_suggestion_id: Some("s2".to_owned()),
    }
}

fn sample_suggestion(state: SuggestionState, owner_id: Option<&str>) -> Suggestion {
    Suggestion {
        suggestion_id: "s1".to_owned(),
        timeline_id: "t1".to_owned(),
        owner_id: owner_id.map(str::to_owned),
        state,
        kind: SuggestionKind::Edit,
        target_event_id: Some("e1".to_owned()),
        payload: SuggestionPayload::Edit(EditPayload {
            title: Some("Better title".to_owned()),
            description: None,
            date: None,
            sources: None,
            significance: None,
        }),
        submitted_by: "account-9".to_owned(),
        submitted_at: ts("2026-11-08T10:00:00Z"),
        reviewed_by: None,
        reviewed_at: None,
        review_note: None,
        report_count: 0,
        flagged: false,
    }
}

#[test]
fn account_item() {
    let account = Account {
        sub: "abc-123".to_owned(),
        display_name: "Ada".to_owned(),
        created_at: ts("2026-11-07T12:30:00Z"),
        last_seen_at: ts("2026-11-08T08:00:00Z"),
        refresh_token_ciphertext: Some("kms:ciphertext".to_owned()),
    };

    let item = account.to_item();
    assert_eq!(item["itemType"], s("ACCOUNT"));
    assert_eq!(item["PK"], s("ACCOUNT#abc-123"));
    assert_eq!(item["SK"], s("PROFILE"));
    assert_eq!(item["displayName"], s("Ada"));
    assert_eq!(item["createdAt"], s("2026-11-07T12:30:00.000Z"));
    assert_eq!(item["lastSeenAt"], s("2026-11-08T08:00:00.000Z"));
    assert_eq!(item["refreshTokenCiphertext"], s("kms:ciphertext"));
    assert_eq!(Account::from_item(item).unwrap(), account);
}

#[test]
fn account_without_a_refresh_token_omits_the_attribute() {
    let account = Account {
        sub: "abc-123".to_owned(),
        display_name: "Ada".to_owned(),
        created_at: ts("2026-11-07T12:30:00Z"),
        last_seen_at: ts("2026-11-08T08:00:00Z"),
        refresh_token_ciphertext: None,
    };

    let item = account.to_item();
    assert!(!item.contains_key("refreshTokenCiphertext"));
    assert_eq!(Account::from_item(item).unwrap(), account);
}

#[test]
fn collection_item_and_gsi2() {
    let collection = Collection {
        id: "c1".to_owned(),
        title: "A collection".to_owned(),
        description: "Nested things".to_owned(),
        parent_collection_id: Some("pc1".to_owned()),
        created_at: ts("2026-11-07T12:30:00Z"),
        created_by: "owner-1".to_owned(),
    };

    let item = collection.to_item();
    assert_eq!(item["itemType"], s("COLLECTION"));
    assert_eq!(item["PK"], s("COLLECTION#c1"));
    assert_eq!(item["SK"], s("META"));
    assert_eq!(item["parentCollectionId"], s("pc1"));
    assert_eq!(item["GSI2PK"], s("pc1"));
    assert_eq!(item["GSI2SK"], s("C#c1"));
    assert_eq!(Collection::from_item(item).unwrap(), collection);

    let top_level = Collection {
        parent_collection_id: None,
        ..collection
    };
    let item = top_level.to_item();
    assert_eq!(item["GSI2PK"], s("ROOT"));
    assert!(!item.contains_key("parentCollectionId"));
}

#[test]
fn timeline_item_matches_spec_layout() {
    let timeline = sample_timeline();
    let item = timeline.to_item();

    assert_eq!(item["itemType"], s("TIMELINE"));
    assert_eq!(item["PK"], s("TIMELINE#t1"));
    assert_eq!(item["SK"], s("META"));
    assert_eq!(item["title"], s("A timeline"));
    assert_eq!(item["description"], s("About things"));
    assert_eq!(item["ownerId"], s("owner-1"));
    assert!(!item.contains_key("collectionId"));
    assert_eq!(item["eventCount"], n("2"));
    assert_eq!(item["upvotes"], n("3"));
    assert_eq!(item["downvotes"], n("1"));
    assert_eq!(item["createdAt"], s("2026-11-07T12:30:00.000Z"));
    assert_eq!(item["updatedAt"], s("2026-11-08T09:00:00.000Z"));
    assert_eq!(item["reportCount"], n("0"));
    assert_eq!(item["flagged"], AttributeValue::Bool(false));
    assert_eq!(item["published"], AttributeValue::Bool(true));
    assert_eq!(item["GSI2PK"], s("ROOT"));
    assert_eq!(item["GSI2SK"], s("T#2026-11-07T12:30:00.000Z#t1"));

    let AttributeValue::M(cover) = &item["coverImage"] else {
        panic!("coverImage must be a map");
    };
    assert_eq!(cover["key"], s("cover/t1/abc.jpg"));
    assert_eq!(cover["width"], n("1600"));
    assert_eq!(cover["height"], n("900"));
    assert_eq!(cover["credit"], s("A photographer"));

    assert_eq!(Timeline::from_item(item).unwrap(), timeline);

    let collected = Timeline {
        collection_id: Some("c1".to_owned()),
        cover_image: None,
        ..timeline
    };
    let item = collected.to_item();
    assert_eq!(item["GSI2PK"], s("c1"));
    assert_eq!(item["collectionId"], s("c1"));
    assert!(!item.contains_key("coverImage"));
    assert_eq!(Timeline::from_item(item).unwrap(), collected);
}

#[test]
fn a_timeline_written_before_publishing_existed_decodes_as_published() {
    // Rows created before the `published` attribute existed must stay live: an
    // absent attribute means published, never a silent unpublish.
    let timeline = sample_timeline();
    let mut item = timeline.to_item();
    item.remove("published");

    let decoded = Timeline::from_item(item).expect("the older row still decodes");
    assert!(decoded.published, "absent means published");

    let unpublished = Timeline {
        published: false,
        ..timeline
    };
    let item = unpublished.to_item();
    assert_eq!(item["published"], AttributeValue::Bool(false));
    assert!(!Timeline::from_item(item).unwrap().published);
}

#[test]
fn event_item_lives_in_timeline_partition() {
    let event = sample_event();
    let item = event.to_item();

    assert_eq!(item["itemType"], s("EVENT"));
    assert_eq!(item["PK"], s("TIMELINE#t1"));
    assert_eq!(item["SK"], s("EVENT#2026-11-07T12:30:00.000Z#e1"));
    assert_eq!(item["date"], s("2026-11-07"));
    assert_eq!(item["significance"], s("crucial"));
    assert_eq!(item["upvotes"], n("2"));
    assert_eq!(item["downvotes"], n("0"));
    assert_eq!(item["reportCount"], n("0"));
    assert_eq!(item["flagged"], AttributeValue::Bool(false));
    assert_eq!(item["createdBy"], s("owner-1"));
    assert_eq!(item["lastEditedBy"], s("owner-2"));
    assert_eq!(item["lastEditSuggestionId"], s("s2"));

    let AttributeValue::L(sources) = &item["sources"] else {
        panic!("sources must be a list");
    };
    assert_eq!(sources.as_slice(), [s("https://example.com/a")].as_slice());

    assert_eq!(Event::from_item(item).unwrap(), event);
}

#[test]
fn legacy_emphasis_attributes_still_decode() {
    let mut item = sample_event().to_item();
    item.remove("significance");
    item.insert("emphasis".to_owned(), s("featured"));

    assert_eq!(
        Event::from_item(item).unwrap().significance,
        Significance::Crucial
    );
}

#[test]
fn vote_direction_is_numeric() {
    let timeline_vote = Vote {
        account_id: "a1".to_owned(),
        timeline_id: "t1".to_owned(),
        event_id: None,
        direction: VoteDirection::Up,
        created_at: ts("2026-11-08T10:00:00Z"),
    };
    let item = timeline_vote.to_item();
    assert_eq!(item["itemType"], s("VOTE"));
    assert_eq!(item["PK"], s("VOTE#a1"));
    assert_eq!(item["SK"], s("T#t1"));
    assert_eq!(item["direction"], n("1"));
    assert_eq!(Vote::from_item(item).unwrap(), timeline_vote);

    let event_vote = Vote {
        event_id: Some("e1".to_owned()),
        direction: VoteDirection::Down,
        ..timeline_vote
    };
    let item = event_vote.to_item();
    assert_eq!(item["SK"], s("T#t1#EVENT#e1"));
    assert_eq!(item["direction"], n("-1"));
    assert_eq!(Vote::from_item(item).unwrap(), event_vote);
}

#[test]
fn pending_suggestion_sets_the_sparse_owner_queue_gsi() {
    let suggestion = sample_suggestion(SuggestionState::Pending, Some("owner-1"));
    let item = suggestion.to_item();

    assert_eq!(item["itemType"], s("SUGGESTION"));
    assert_eq!(item["PK"], s("TIMELINE#t1"));
    assert_eq!(item["SK"], s("SUGGESTION#s1"));
    assert_eq!(item["state"], s("PENDING"));
    assert_eq!(item["kind"], s("EDIT"));
    assert_eq!(item["targetEventId"], s("e1"));
    assert_eq!(item["submittedBy"], s("account-9"));
    assert_eq!(item["submittedAt"], s("2026-11-08T10:00:00.000Z"));
    assert_eq!(item["GSI1PK"], s("OWNER#owner-1#PENDING"));
    assert_eq!(item["GSI1SK"], s("2026-11-08T10:00:00.000Z"));
    assert_eq!(item["reportCount"], n("0"));
    assert_eq!(item["flagged"], AttributeValue::Bool(false));

    let AttributeValue::M(payload) = &item["payload"] else {
        panic!("payload must be a map");
    };
    // EDIT payload is sparse: only proposed fields are present.
    assert_eq!(payload["title"], s("Better title"));
    assert_eq!(payload.len(), 1);

    let read_back = Suggestion::from_item(item).unwrap();
    assert_eq!(read_back.owner_id, None);
    assert_eq!(read_back.payload, suggestion.payload);
}

#[test]
fn non_pending_suggestion_drops_the_owner_queue_attributes() {
    for state in [SuggestionState::Approved, SuggestionState::Rejected] {
        let item = sample_suggestion(state, Some("owner-1")).to_item();
        assert!(!item.contains_key("GSI1PK"));
        assert!(!item.contains_key("GSI1SK"));
    }
}

#[test]
fn suggestions_written_before_reporting_decode_with_zero_counts() {
    let suggestion = sample_suggestion(SuggestionState::Pending, Some("owner-1"));
    let mut item = suggestion.to_item();
    item.remove("reportCount");
    item.remove("flagged");

    let read_back = Suggestion::from_item(item).expect("legacy suggestion decodes");
    assert_eq!(read_back.report_count, 0);
    assert!(!read_back.flagged);
}

#[test]
fn append_suggestion_carries_the_full_proposed_event() {
    let suggestion = Suggestion {
        kind: SuggestionKind::Append,
        target_event_id: None,
        payload: SuggestionPayload::Append(AppendPayload {
            date: "2026-11-09".to_owned(),
            title: "Proposed event".to_owned(),
            description: "Proposed description".to_owned(),
            sources: vec!["https://example.com/new".to_owned()],
            significance: Significance::Regular,
        }),
        ..sample_suggestion(SuggestionState::Pending, None)
    };

    let item = suggestion.to_item();
    let AttributeValue::M(payload) = &item["payload"] else {
        panic!("payload must be a map");
    };
    assert_eq!(payload["date"], s("2026-11-09"));
    assert_eq!(payload["title"], s("Proposed event"));
    assert_eq!(payload["significance"], s("regular"));
    assert_eq!(
        Suggestion::from_item(item).unwrap().payload,
        suggestion.payload
    );
}

#[test]
fn report_item() {
    let report = Report {
        target_type: ReportTargetType::Timeline,
        target_id: "t1".to_owned(),
        account_id: "a1".to_owned(),
        created_at: ts("2026-11-08T10:00:00Z"),
    };
    let item = report.to_item();

    assert_eq!(item["itemType"], s("REPORT"));
    assert_eq!(item["PK"], s("REPORT#TIMELINE#t1"));
    assert_eq!(item["SK"], s("ACCOUNT#a1"));
    assert_eq!(item["targetType"], s("TIMELINE"));
    assert_eq!(item["targetId"], s("t1"));
    assert_eq!(Report::from_item(item).unwrap(), report);
}

#[test]
fn ratelimit_item_uses_ttl() {
    let limit = RateLimit {
        partition: RateLimitPartition::Account("a1".to_owned()),
        action: "vote".to_owned(),
        window_bucket: "2026-11-08T10:00".to_owned(),
        count: 4,
        expires_at: 1_800_000_000,
    };
    let item = limit.to_item();

    assert_eq!(item["itemType"], s("RATELIMIT"));
    assert_eq!(item["PK"], s("RL#ACCOUNT#a1"));
    assert_eq!(item["SK"], s("vote#2026-11-08T10:00"));
    assert_eq!(item["count"], n("4"));
    assert_eq!(item["expiresAt"], n("1800000000"));
    assert_eq!(RateLimit::from_item(item).unwrap(), limit);

    let ip_limit = RateLimit {
        partition: RateLimitPartition::IpHash("deadbeef".to_owned()),
        ..limit
    };
    assert_eq!(ip_limit.to_item()["PK"], s("RL#IP#deadbeef"));
}

#[test]
fn suggestcount_item() {
    let count = SuggestCount {
        timeline_id: "t1".to_owned(),
        account_id: "a1".to_owned(),
        pending: 3,
    };
    let item = count.to_item();

    assert_eq!(item["itemType"], s("SUGGESTCOUNT"));
    assert_eq!(item["PK"], s("TIMELINE#t1"));
    assert_eq!(item["SK"], s("SUGGESTCOUNT#a1"));
    assert_eq!(item["pending"], n("3"));
    assert_eq!(SuggestCount::from_item(item).unwrap(), count);
}

#[test]
fn session_item_uses_ttl() {
    let session = Session {
        id: "sess-1".to_owned(),
        account_id: "a1".to_owned(),
        expires_at: 1_800_000_000,
    };
    let item = session.to_item();

    assert_eq!(item["itemType"], s("SESSION"));
    assert_eq!(item["PK"], s("SESSION#sess-1"));
    assert_eq!(item["SK"], s("META"));
    assert_eq!(item["accountId"], s("a1"));
    assert_eq!(item["expiresAt"], n("1800000000"));
    assert_eq!(Session::from_item(item).unwrap(), session);
}

#[test]
fn auth_state_item_uses_ttl_and_hides_the_verifier() {
    let state = AuthState {
        state: "state-1".to_owned(),
        code_verifier: "verifier-1".to_owned(),
        redirect_uri: "http://localhost:8081/api/auth/callback".to_owned(),
        created_at: ts("2026-11-07T12:30:00Z"),
        expires_at: 1_800_000_000,
    };
    let item = state.to_item();

    assert_eq!(item["itemType"], s("AUTHSTATE"));
    assert_eq!(item["PK"], s("AUTHSTATE#state-1"));
    assert_eq!(item["SK"], s("PKCE"));
    assert_eq!(item["codeVerifier"], s("verifier-1"));
    assert_eq!(item["expiresAt"], n("1800000000"));
    assert_eq!(AuthState::from_item(item).unwrap(), state);
}

#[test]
fn item_type_discriminator_is_checked_on_read() {
    let mut item = sample_event().to_item();
    item.insert("itemType".to_owned(), s("TIMELINE"));
    assert!(Event::from_item(item).is_err());
}

#[test]
fn missing_required_attribute_is_an_error() {
    let mut item = sample_event().to_item();
    item.remove("sources");
    assert!(Event::from_item(item).is_err());
}

#[test]
fn item_type_strings_are_stable() {
    assert_eq!(ItemType::Account.as_str(), "ACCOUNT");
    assert_eq!(ItemType::Collection.as_str(), "COLLECTION");
    assert_eq!(ItemType::Timeline.as_str(), "TIMELINE");
    assert_eq!(ItemType::Event.as_str(), "EVENT");
    assert_eq!(ItemType::Vote.as_str(), "VOTE");
    assert_eq!(ItemType::Suggestion.as_str(), "SUGGESTION");
    assert_eq!(ItemType::Report.as_str(), "REPORT");
    assert_eq!(ItemType::RateLimit.as_str(), "RATELIMIT");
    assert_eq!(ItemType::SuggestCount.as_str(), "SUGGESTCOUNT");
    assert_eq!(ItemType::Session.as_str(), "SESSION");
    assert_eq!(ItemType::AuthState.as_str(), "AUTHSTATE");
}

// Keeps `AttrMap` in the public surface exercised by an external user.
#[test]
fn item_maps_are_plain_attribute_maps() {
    let map: AttrMap = sample_timeline().to_item();
    assert!(map.contains_key("PK"));
}
